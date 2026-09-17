// Per-session terminal stream authority for the worker's view owner: it
// minimizes the registry's live viewer geometry, mints every stream id, drives
// SessionManager.applyTerminalStreamState, and classifies the outcome into the
// TerminalStreamState the shared registry reads back. Desires coalesce per
// session, so a dragged pane resize cannot queue one keeper resize per
// intermediate width. Membership itself lives in the registry, never here.

import { randomUUID } from "node:crypto";
import { TerminalViewStatus } from "@roost/shared/proto/sync_pb";
import { log } from "@roost/shared/log";
import {
	truncateTerminalReason,
	type TerminalStreamState,
	type TerminalUnavailablePolicy,
	type TerminalViewGeometrySet,
} from "@roost/shared/terminal-view";
import { minimumTerminalGeometry, type TerminalGeometry } from "@roost/shared/viewport";
import type { SessionManager } from "./session-manager.ts";
import type { WorkerTerminalStreamResult } from "./session-terminal-state.ts";
import { TERMINAL_REQUEST_BUDGET_CAP_MS } from "./transport/coord-link-constants.ts";
import type { TerminalRequestBudget } from "./transport/coord-link-types.ts";
import { monoNowMs } from "./util/mono.ts";

export interface TerminalViewStreamsOptions {
	sessions(): SessionManager;
	/** The registry's live/retained geometry set for one session. */
	geometries(sessionId: string): TerminalViewGeometrySet;
	broadcast(sessionId: string, status: TerminalViewStatus, message: string): void;
	/** Membership or effective geometry changed: republish the projection. */
	publish(sessionId: string): void;
}

interface StreamSession extends TerminalStreamState {
	/** Stream id of the apply currently awaiting the control lane. */
	inFlight: string | null;
	latest: { geometry: TerminalGeometry | null; retry: number } | null;
}

export class TerminalViewStreams {
	private readonly streams = new Map<string, StreamSession>();
	private disposed = false;

	constructor(private readonly options: TerminalViewStreamsOptions) {}

	state(sessionId: string): TerminalStreamState | null {
		return this.streams.get(sessionId) ?? null;
	}

	sessionIds(): readonly string[] {
		return [...this.streams.keys()];
	}

	/** True when this changed the session's stream, which is the registry's
	 * signal that the decision was already broadcast to every live view. */
	recompute(sessionId: string): boolean {
		const { live, retained } = this.options.geometries(sessionId);
		const session = this.session(sessionId);
		// A session whose every viewer is parked HOLDS its last geometry: park
		// absorbs reconnect wobble, so a solo viewer's socket blip must not
		// re-mint the stream or resize the PTY. Losing membership entirely is
		// what disables it.
		if (live.length === 0 && retained > 0) {
			this.options.publish(sessionId);
			return false;
		}
		const effective = minimumTerminalGeometry(live);
		if (
			effective?.cols === session.effective?.cols
			&& effective?.rows === session.effective?.rows
		) {
			this.options.publish(sessionId);
			return false;
		}
		session.effective = effective;
		this.desire(sessionId, session, effective, 0);
		this.options.publish(sessionId);
		return true;
	}

	redrive(sessionId: string): void {
		const session = this.streams.get(sessionId);
		if (session?.effective && session.unavailablePolicy !== "never") {
			this.desire(sessionId, session, session.effective, 0);
		}
	}

	closeSession(sessionId: string): void {
		const session = this.streams.get(sessionId);
		if (!session) return;
		session.effective = null;
		session.latest = null;
		session.streamId = "";
		this.streams.delete(sessionId);
		this.options.publish(sessionId);
	}

	dispose(): void {
		this.disposed = true;
		this.streams.clear();
	}

	private session(sessionId: string): StreamSession {
		let session = this.streams.get(sessionId);
		if (!session) {
			session = {
				effective: null,
				streamId: "",
				unavailable: false,
				unavailableReason: "",
				unavailablePolicy: "heartbeat",
				inFlight: null,
				latest: null,
			};
			this.streams.set(sessionId, session);
		}
		return session;
	}

	private desire(
		sessionId: string,
		session: StreamSession,
		geometry: TerminalGeometry | null,
		retry: number,
	): void {
		session.streamId = randomUUID();
		session.unavailable = false;
		session.unavailableReason = "";
		session.unavailablePolicy = "heartbeat";
		session.latest = { geometry, retry };
		log.info("terminal-view", "stream_desired", {
			session_id: sessionId,
			stream_id: session.streamId,
			enabled: geometry !== null,
			cols: geometry?.cols ?? 0,
			rows: geometry?.rows ?? 0,
			retry,
		});
		// Every live view learns this stream BEFORE any of its cells exist: the
		// apply below is what installs the stream, and it runs after this returns.
		this.options.broadcast(sessionId, TerminalViewStatus.ACCEPTED, "");
		void this.drive(sessionId, session);
	}

	private async drive(sessionId: string, session: StreamSession): Promise<void> {
		if (session.inFlight || !session.latest) return;
		const work = session.latest;
		const streamId = session.streamId;
		session.latest = null;
		session.inFlight = streamId;
		try {
			// Synchronous up to here on purpose: the stream the views were just
			// told about is installed before any other work can interleave.
			const result = await this.options.sessions().applyTerminalStreamState({
				requestId: randomUUID(),
				sessionId,
				streamId,
				enabled: work.geometry !== null,
				cols: work.geometry?.cols ?? 0,
				rows: work.geometry?.rows ?? 0,
				budget: this.budget(sessionId, streamId),
			});
			this.classify(sessionId, session, streamId, work.retry, result);
		} catch (error) {
			if (session.streamId === streamId) {
				this.unavailable(
					sessionId,
					session,
					error instanceof Error ? error.message : String(error),
					"never",
				);
			}
		} finally {
			if (session.inFlight === streamId) session.inFlight = null;
			if (session.latest) void this.drive(sessionId, session);
		}
	}

	private classify(
		sessionId: string,
		session: StreamSession,
		streamId: string,
		retry: number,
		result: WorkerTerminalStreamResult,
	): void {
		if (session.streamId !== streamId) return;
		if (result.status === "committed") return;
		// One retry, and only for a failure that provably never wrote: the
		// admission lane refuses while another transaction owns the channel.
		if (result.failure === "retryable_pre_write" && retry === 0) {
			this.desire(sessionId, session, session.effective, 1);
			return;
		}
		// A trap the keeper boundary caused is the one failure the worker can
		// repair itself: the next apply re-proves the core from keeper history
		// (session-core-reprove.ts). One attempt per trap, driven by the trap and
		// not by a timer or a heartbeat door; if it fails the verdict stays
		// fail-closed.
		if (result.failure === "core_failed" && retry === 0 && session.effective) {
			this.desire(sessionId, session, session.effective, 1);
			return;
		}
		this.unavailable(
			sessionId,
			session,
			result.reason || "terminal stream is unavailable",
			result.failure === "retryable_pre_write" ? "heartbeat" : "never",
		);
	}

	private unavailable(
		sessionId: string,
		session: StreamSession,
		message: string,
		policy: TerminalUnavailablePolicy,
	): void {
		if (!session.effective) return;
		session.unavailable = true;
		session.unavailableReason = truncateTerminalReason(message);
		session.unavailablePolicy = policy;
		log.warn("terminal-view", "stream_unavailable", {
			session_id: sessionId,
			stream_id: session.streamId,
			policy,
			reason: session.unavailableReason,
		});
		this.options.broadcast(sessionId, TerminalViewStatus.UNAVAILABLE, session.unavailableReason);
	}

	/** The worker is both requester and executor here, so the budget is the
	 * ordinary terminal-control ceiling and the request stays current while this
	 * desire still owns the session's stream. */
	private budget(sessionId: string, streamId: string): TerminalRequestBudget {
		const startedAtMono = monoNowMs();
		return {
			remainingMs: () => TERMINAL_REQUEST_BUDGET_CAP_MS - (monoNowMs() - startedAtMono),
			isCurrentConnection: () =>
				!this.disposed && this.streams.get(sessionId)?.streamId === streamId,
		};
	}
}
