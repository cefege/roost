/**
 * Relay-backed guest protocol client.
 *
 * Socket lifecycle and request correlation stay here; canonical host-frame
 * reduction lives in the transport-neutral {@link SessionReplica}.
 */

import { type CollabUiResponseValue, type HostFrame } from "@oh-my-pi/pi-wire";
import { importRoomKey } from "./codec";
import { COLLAB_PROTO, encodeBase64Url, parseCollabLink } from "./link";
import { CollabSocket } from "./socket";
import {
	SessionReplica,
	type GuestSnapshot,
	type TranscriptResult,
} from "./session-replica";

export { SessionReplica };
export type {
	ActiveTool,
	ConnectionPhase,
	GuestSnapshot,
	Notice,
	SessionActivity,
	SessionGoal,
	SessionGoalStatus,
	SessionReplicaEffect,
	SessionRuleSummary,
	SessionTodo,
	SessionTodoStatus,
	TranscriptResult,
} from "./session-replica";

const TRANSCRIPT_TIMEOUT_MS = 10_000;
/** Mirrors the TUI guest's WELCOME_TIMEOUT_MS: a host that never answers hello ends the join. */
const WELCOME_TIMEOUT_MS = 30_000;
/** Mirrors the TUI guest's SNAPSHOT_PROGRESS_TIMEOUT_MS: every snapshot chunk must make progress. */
const SNAPSHOT_PROGRESS_TIMEOUT_MS = 30_000;

/** Transport-neutral commands consumed by the session presentation. */
export interface SessionActions {
	sendPrompt(text: string): void;
	sendUiResponse(reqId: number, value?: CollabUiResponseValue): void;
	sendAbort(): void;
	sendAgentCmd(cmd: "chat" | "kill" | "revive", agentId: string, text?: string): void;
	fetchTranscript(agentId: string, fromByte: number): Promise<TranscriptResult | null>;
}

interface PendingTranscript {
	resolve: (result: TranscriptResult | null) => void;
	timer: Timer;
}

export class GuestClient implements SessionActions {
	readonly #socket: CollabSocket;
	readonly #name: string;
	/** base64url write token from a full link; absent when joined via a view link. */
	readonly #writeToken: string | undefined;
	readonly #replica = new SessionReplica();
	readonly #pendingTranscripts = new Map<number, PendingTranscript>();
	#reqSeq = 0;
	#everConnected = false;
	#welcomed = false;
	#welcomeTimer: Timer | null = null;
	#snapshotProgressTimer: Timer | null = null;

	/** @throws Error when the link does not parse. */
	constructor(link: string, displayName: string) {
		const parsed = parseCollabLink(link);
		if ("error" in parsed) throw new Error(parsed.error);
		this.#name = displayName;
		this.#writeToken = parsed.writeToken ? encodeBase64Url(parsed.writeToken) : undefined;
		this.#socket = new CollabSocket({ wsUrl: parsed.wsUrl, role: "guest", key: importRoomKey(parsed.key) });
		this.#socket.onOpen = () => this.#handleOpen();
		this.#socket.onFrame = frame => this.#handleFrame(frame);
		this.#socket.onControl = msg => {
			if (msg.t === "room-closed") this.#end("room closed");
		};
		this.#socket.onClose = (reason, willReconnect) => this.#handleClose(reason, willReconnect);
	}

	connect(): void {
		if (this.#replica.getSnapshot().phase === "ended") this.#replica.setPhase("connecting");
		this.#socket.connect();
		if (!this.#welcomed && this.#welcomeTimer === null) {
			this.#welcomeTimer = setTimeout(() => {
				this.#welcomeTimer = null;
				if (!this.#welcomed) this.#end("timed out waiting for the host's welcome");
			}, WELCOME_TIMEOUT_MS);
		}
	}

	close(): void {
		this.#clearWelcomeTimer();
		this.#clearSnapshotProgressTimer();
		this.#socket.close();
	}

	subscribe(listener: () => void): () => void {
		return this.#replica.subscribe(listener);
	}

	getSnapshot(): GuestSnapshot {
		return this.#replica.getSnapshot();
	}

	sendPrompt(text: string): void {
		this.#socket.send({ t: "prompt", text });
	}

	sendUiResponse(reqId: number, value?: CollabUiResponseValue): void {
		this.#socket.send({ t: "ui-response", reqId, value });
		this.#replica.resolveUiRequest(reqId);
	}

	sendAbort(): void {
		this.#socket.send({ t: "abort" });
	}

	sendAgentCmd(cmd: "chat" | "kill" | "revive", agentId: string, text?: string): void {
		this.#socket.send({ t: "agent-cmd", cmd, agentId, text });
	}

	/**
	 * Incremental subagent-transcript read. Resolves a {@link TranscriptResult}
	 * (`rows` or terminal `error`), or `null` on transient failure (10s timeout,
	 * session end) where re-polling from the same cursor is correct.
	 */
	fetchTranscript(agentId: string, fromByte: number): Promise<TranscriptResult | null> {
		const reqId = ++this.#reqSeq;
		const { promise, resolve } = Promise.withResolvers<TranscriptResult | null>();
		const timer = setTimeout(() => {
			this.#pendingTranscripts.delete(reqId);
			resolve(null);
		}, TRANSCRIPT_TIMEOUT_MS);
		this.#pendingTranscripts.set(reqId, { resolve, timer });
		this.#socket.send({ t: "fetch-transcript", reqId, agentId, fromByte });
		return promise;
	}

	/** Compatibility seam: synthetic frames use the same public reducer as live relay frames. */
	applyFrameForTest(frame: HostFrame): void {
		this.#handleFrame(frame);
	}

	#handleOpen(): void {
		this.#socket.send({ t: "hello", proto: COLLAB_PROTO, name: this.#name, writeToken: this.#writeToken });
		this.#replica.setPhase(this.#everConnected ? "reconnecting" : "waiting");
		this.#everConnected = true;
	}

	#handleClose(reason: string, willReconnect: boolean): void {
		this.#clearSnapshotProgressTimer();
		if (this.#replica.getSnapshot().phase === "ended") return;
		if (willReconnect) {
			this.#replica.setPhase("reconnecting");
			return;
		}
		this.#end(reason);
	}

	#end(reason: string): void {
		if (this.#replica.getSnapshot().phase === "ended") return;
		this.#replica.setPhase("ended", reason);
		this.#shutdownEndedTransport();
	}

	#shutdownEndedTransport(): void {
		this.#clearWelcomeTimer();
		this.#clearSnapshotProgressTimer();
		for (const [, pending] of this.#pendingTranscripts) {
			clearTimeout(pending.timer);
			pending.resolve(null);
		}
		this.#pendingTranscripts.clear();
		this.#socket.close();
	}

	#clearWelcomeTimer(): void {
		if (this.#welcomeTimer !== null) {
			clearTimeout(this.#welcomeTimer);
			this.#welcomeTimer = null;
		}
	}

	#armSnapshotProgressTimer(): void {
		this.#clearSnapshotProgressTimer();
		this.#snapshotProgressTimer = setTimeout(() => {
			this.#snapshotProgressTimer = null;
			this.#end("timed out waiting for the host's session snapshot");
		}, SNAPSHOT_PROGRESS_TIMEOUT_MS);
	}

	#clearSnapshotProgressTimer(): void {
		if (this.#snapshotProgressTimer !== null) {
			clearTimeout(this.#snapshotProgressTimer);
			this.#snapshotProgressTimer = null;
		}
	}

	#handleFrame(frame: HostFrame): void {
		const previousPhase = this.#replica.getSnapshot().phase;
		const effect = this.#replica.applyFrame(frame);
		if (effect?.kind === "transcript") {
			const pending = this.#pendingTranscripts.get(effect.reqId);
			if (pending) {
				this.#pendingTranscripts.delete(effect.reqId);
				clearTimeout(pending.timer);
				pending.resolve(effect.result);
			}
		}

		const phase = this.#replica.getSnapshot().phase;
		if (phase === "ended") {
			if (previousPhase !== "ended") this.#shutdownEndedTransport();
			return;
		}
		if (frame.t === "welcome") {
			this.#welcomed = true;
			this.#clearWelcomeTimer();
			if (frame.entryCount === 0) this.#clearSnapshotProgressTimer();
			else this.#armSnapshotProgressTimer();
		} else if (frame.t === "snapshot-chunk") {
			if (frame.final) this.#clearSnapshotProgressTimer();
			else this.#armSnapshotProgressTimer();
		}
	}
}
