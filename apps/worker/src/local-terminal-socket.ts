// Frame routing for the worker's loopback terminal sockets. It verifies each
// hello against the in-memory grant store, registers the socket as a terminal
// view owner socket, and carries view commands, input, scrollback reads and
// cell frames for the granted sessions only. Input takes the ordinary
// coordinator-facing write path, so the keeper-update write freeze and every
// admission fence still apply to a local keystroke.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { log } from "@roost/shared/log";
import {
	LocalTerminalClientFrameSchema,
	LocalTerminalClosedSchema,
	LocalTerminalReadySchema,
	LocalTerminalServerFrameSchema,
	type LocalScrollbackRequest,
	type LocalTerminalHello,
	type LocalTerminalServerFrame,
} from "@roost/shared/proto/local_terminal_pb";
import {
	InputAcceptedSchema,
	InputAmbiguousSchema,
	InputRejectedSchema,
	type InputCommand,
} from "@roost/shared/proto/sync_pb";
import { KEEPER_MAX_INPUT_BYTES } from "./keeper/protocol.ts";
import type {
	LocalTerminalSocket,
	LocalTerminalSocketHandlers,
} from "./local-ui-server.ts";
import type { LocalTerminalGrant, LocalTerminalGrantStore } from "./local-terminal-grants.ts";
import { readLocalScrollback } from "./local-terminal-scrollback.ts";
import type { SessionManager } from "./session-manager.ts";
import type { WorkerInputResult } from "./session-terminal-control.ts";
import type { TerminalViewOwner } from "./terminal-view-owner.ts";
import type { LocalViewTransport } from "./terminal-view-owner-screen.ts";
import { TERMINAL_REQUEST_BUDGET_CAP_MS } from "./transport/coord-link-constants.ts";
import type { TerminalRequestBudget } from "./transport/coord-link-types.ts";
import { monoNowMs } from "./util/mono.ts";

/** Bytes the browser has left unread before its socket is judged unable to
 * drain. One full 256×256 grid is well under this, so a pane that is merely
 * busy recovers while a wedged one is dropped instead of queueing forever. */
const MAX_BACKPRESSURE_BYTES = 4 * 1024 * 1024;

export interface LocalTerminalSocketDeps {
	sessions(): SessionManager;
	grants: LocalTerminalGrantStore;
	viewOwner: TerminalViewOwner;
	workerFingerprint: string;
}

interface LocalTerminalSession {
	readonly socket: LocalTerminalSocket;
	/** Unique per accepted socket: the browser fences pending input on it. */
	generation: bigint;
	grant: LocalTerminalGrant | null;
	/** Bytes the socket buffered since its last clean write. */
	backpressuredBytes: number;
	closing: boolean;
}

export class LocalTerminalSockets implements LocalTerminalSocketHandlers {
	private readonly sockets = new Map<string, LocalTerminalSession>();
	private generations = 0n;

	constructor(private readonly deps: LocalTerminalSocketDeps) {}

	onOpen(socket: LocalTerminalSocket): void {
		this.sockets.set(socket.socketId, {
			socket,
			generation: 0n,
			grant: null,
			backpressuredBytes: 0,
			closing: false,
		});
	}

	onMessage(socket: LocalTerminalSocket, data: Uint8Array): void {
		const session = this.sockets.get(socket.socketId);
		if (!session) return;
		let frame;
		try {
			frame = fromBinary(LocalTerminalClientFrameSchema, data);
		} catch (error) {
			log.warn("local-terminal", "frame_decode_failed", {
				socket_id: socket.socketId,
				error: error instanceof Error ? error.message : String(error),
			});
			this.close(session, "undecodable frame");
			return;
		}
		const client = frame.frame;
		if (client.case === "hello") {
			this.accept(session, client.value);
			return;
		}
		// Nothing is served before the capability is proven, and a second hello
		// would silently re-point a live socket's membership.
		if (!session.grant) {
			this.close(session, "hello required");
			return;
		}
		switch (client.case) {
			case "terminalView":
				this.deps.viewOwner.handleViewCommand(socket.socketId, client.value);
				return;
			case "terminalResync":
				this.deps.viewOwner.handleResync(socket.socketId, client.value);
				return;
			case "input":
				void this.write(session, client.value);
				return;
			case "scrollback":
				void this.serveScrollback(session, client.value);
				return;
			case undefined:
				return;
		}
	}

	onClose(socket: LocalTerminalSocket): void {
		const session = this.sockets.get(socket.socketId);
		if (!session) return;
		this.sockets.delete(socket.socketId);
		if (session.grant) this.deps.viewOwner.closeSocket(socket.socketId);
	}

	/** The coordinator revoked this device's key: its grants are already gone,
	 * and a socket holding one must not outlive them. */
	revokeDevice(deviceFingerprint: string): void {
		this.deps.grants.revokeDevice(deviceFingerprint);
		for (const session of [...this.sockets.values()]) {
			if (session.grant?.deviceFingerprint !== deviceFingerprint) continue;
			this.close(session, "local terminal grant revoked");
		}
	}

	private accept(session: LocalTerminalSession, hello: LocalTerminalHello): void {
		if (session.grant) {
			this.close(session, "duplicate hello");
			return;
		}
		const verdict = this.deps.grants.verify({
			grantId: hello.grantId,
			secret: hello.secret,
			tabId: hello.tabId,
			deviceFingerprint: hello.deviceFingerprint,
		});
		if (!verdict.ok) {
			log.warn("local-terminal", "hello_refused", {
				socket_id: session.socket.socketId,
				grant_id: hello.grantId,
				reason: verdict.reason,
			});
			this.close(session, verdict.reason);
			return;
		}
		const grant = verdict.grant;
		this.generations += 1n;
		session.generation = this.generations;
		session.grant = grant;
		this.send(session, {
			case: "ready",
			value: create(LocalTerminalReadySchema, {
				workerFingerprint: this.deps.workerFingerprint,
				sessionIds: [...grant.sessionIds],
				socketGeneration: session.generation,
			}),
		});
		log.info("local-terminal", "hello_accepted", {
			socket_id: session.socket.socketId,
			grant_id: grant.grantId,
			device_fingerprint: grant.deviceFingerprint,
			tab_id: grant.tabId,
			socket_generation: session.generation.toString(),
			sessions: grant.sessionIds.length,
		});
		this.deps.viewOwner.registerLocalSocket({
			socketId: session.socket.socketId,
			deviceFingerprint: grant.deviceFingerprint,
			tabId: grant.tabId,
			allowsSession: (sessionId) => grant.sessionIds.includes(sessionId),
			transport: this.transport(session),
		});
	}

	private transport(session: LocalTerminalSession): LocalViewTransport {
		return {
			kind: "local",
			sendViewState: (frame) => {
				this.send(session, { case: "terminalViewState", value: frame });
			},
			sendCellFrame: (frame) => this.sendCells(session, { case: "cellGrid", value: frame }),
			sendCellChunk: (chunk) => this.sendCells(session, { case: "cellGridChunk", value: chunk }),
			onOverflow: () => { this.close(session, "local delivery overflow"); },
			onViewExpired: () => { this.close(session, "terminal view lease expired"); },
		};
	}

	private async write(session: LocalTerminalSession, command: InputCommand): Promise<void> {
		const grant = session.grant;
		if (!grant) return;
		const sessionId = command.sessionId;
		if (!grant.sessionIds.includes(sessionId)) {
			this.sendInputResult(session, command, {
				status: "rejected",
				writtenBytes: 0,
				reason: "terminal session is unavailable",
			});
			return;
		}
		if (command.data.byteLength === 0) {
			this.sendInputResult(session, command, { status: "accepted", writtenBytes: 0 });
			return;
		}
		// The keeper refuses a larger payload outright, so the door refuses it
		// with a provable pre-write result instead of a silent drop.
		if (command.data.byteLength > KEEPER_MAX_INPUT_BYTES) {
			this.sendInputResult(session, command, {
				status: "rejected",
				writtenBytes: 0,
				reason: "input exceeds 64 KiB",
			});
			return;
		}
		// A protobuf bytes field views the socket's receive buffer, which is
		// recycled on the next read; the keeper admission lane outlives this turn,
		// so ownership must transfer before the write is queued.
		const owned = command.data.slice();
		const result = await this.deps.sessions().writeTerminalInput(
			sessionId,
			command.inputSeq,
			owned,
			this.budget(session),
		);
		this.sendInputResult(session, command, result);
	}

	private async serveScrollback(
		session: LocalTerminalSession,
		request: LocalScrollbackRequest,
	): Promise<void> {
		const grant = session.grant;
		if (!grant) return;
		const value = await readLocalScrollback(
			this.deps.sessions(),
			request,
			(sessionId) => grant.sessionIds.includes(sessionId),
		);
		this.send(session, { case: "scrollback", value });
	}

	/** The local socket is both the requester and the reply path, so a write
	 * stays current exactly while this socket generation is the live one. */
	private budget(session: LocalTerminalSession): TerminalRequestBudget {
		const startedAtMono = monoNowMs();
		const generation = session.generation;
		return {
			remainingMs: () => TERMINAL_REQUEST_BUDGET_CAP_MS - (monoNowMs() - startedAtMono),
			isCurrentConnection: () =>
				session.socket.open
				&& this.sockets.get(session.socket.socketId)?.generation === generation,
		};
	}

	private sendInputResult(
		session: LocalTerminalSession,
		command: InputCommand,
		result: WorkerInputResult,
	): void {
		const common = {
			sessionId: command.sessionId,
			inputSeq: command.inputSeq,
			domainGeneration: session.generation,
		};
		if (result.status === "accepted") {
			this.send(session, {
				case: "inputAccepted",
				value: create(InputAcceptedSchema, { ...common, writtenBytes: result.writtenBytes }),
			});
			return;
		}
		if (result.status === "rejected") {
			this.send(session, {
				case: "inputRejected",
				value: create(InputRejectedSchema, { ...common, reason: result.reason }),
			});
			return;
		}
		this.send(session, {
			case: "inputAmbiguous",
			value: create(InputAmbiguousSchema, {
				...common,
				writtenBytes: result.writtenBytes,
				reason: result.reason,
			}),
		});
	}

	private send(session: LocalTerminalSession, frame: LocalTerminalServerFrame["frame"]): void {
		const bytes = this.encode(session, frame);
		if (bytes) session.socket.send(bytes);
	}

	/** Cells answer the sink contract: "sent" once the socket owns the bytes,
	 * "overflow" once it has stopped draining, so the registry drops this sink
	 * alone instead of growing an unbounded local queue. */
	private sendCells(
		session: LocalTerminalSession,
		frame: LocalTerminalServerFrame["frame"],
	): "sent" | "dropped" | "overflow" {
		const bytes = this.encode(session, frame);
		if (!bytes) return "dropped";
		if (!session.socket.open) return "overflow";
		const written = session.socket.send(bytes);
		if (written > 0) {
			session.backpressuredBytes = 0;
			return "sent";
		}
		if (written < 0) {
			session.backpressuredBytes += bytes.byteLength;
			return session.backpressuredBytes > MAX_BACKPRESSURE_BYTES ? "overflow" : "sent";
		}
		return "dropped";
	}

	private encode(
		session: LocalTerminalSession,
		frame: LocalTerminalServerFrame["frame"],
	): Uint8Array | null {
		try {
			return toBinary(LocalTerminalServerFrameSchema, create(LocalTerminalServerFrameSchema, { frame }));
		} catch (error) {
			log.warn("local-terminal", "frame_encode_failed", {
				socket_id: session.socket.socketId,
				kind: frame.case ?? "unset",
				error: error instanceof Error ? error.message : String(error),
			});
			return null;
		}
	}

	private close(session: LocalTerminalSession, reason: string): void {
		if (session.closing) return;
		session.closing = true;
		log.info("local-terminal", "socket_closing", {
			socket_id: session.socket.socketId,
			reason,
		});
		this.send(session, {
			case: "closed",
			value: create(LocalTerminalClosedSchema, { reason }),
		});
		session.socket.close(1000, reason);
	}
}
