// Browser-command handlers for agent sessions: spawn, compose, answer a
// dialog, abort a turn, page the transcript. Replaces the block that used to
// answer every structured-OMP frame with "Structured OMP is not supported".
//
// Each handler resolves the session, then delegates to its AgentController; the
// controller owns all omp-facing state. Errors come back as rpc-error with the
// real reason (notably "omp not found on PATH"), because a silent fall back to
// a shell session would be the wrong session kind, not a graceful degradation.

import { log } from "@roost/shared";
import type { ClientControlFrame, SessionId } from "@roost/shared/wire";
import type { CoordLink } from "./transport/CoordLink.ts";
import type { SessionManager } from "./session-manager.ts";
import type { AgentController } from "./agent/agent-controller.ts";

interface AgentDeps {
	coordLink: CoordLink;
	sessionMgr: SessionManager;
}

/** Resolve the controller for a session, answering rpc-error when the session
 *  is unknown or is a shell (the SPA routed a transcript command at a
 *  terminal). Returns undefined once it has already replied. */
function resolveAgent(
	sessionId: SessionId,
	request_id: string,
	deps: AgentDeps,
): AgentController | undefined {
	const rec = deps.sessionMgr.getBySessionId(sessionId);
	if (!rec) {
		deps.coordLink.send({ kind: "rpc-error", request_id, message: "unknown session" });
		return undefined;
	}
	if (rec.kind !== "agent") {
		deps.coordLink.send({ kind: "rpc-error", request_id, message: "session is not an agent" });
		return undefined;
	}
	return rec.agent;
}

export function handleSpawnAgent(
	frame: Extract<ClientControlFrame, { kind: "spawn-agent" }>,
	request_id: string,
	deps: AgentDeps,
): void {
	const { coordLink, sessionMgr } = deps;
	// Idempotent by contract: coord fires this on every worker hello for each
	// open agent row, so a session this worker already holds must answer with
	// the live record rather than fork a SECOND omp child against the same
	// session file.
	if (frame.session_id) {
		const existing = sessionMgr.getBySessionId(frame.session_id);
		if (existing) {
			coordLink.send({
				kind: "rpc-ok",
				request_id,
				data: { session_id: existing.sessionId, channel_id: existing.channelId },
			});
			return;
		}
	}
	sessionMgr
		.spawnAgent(frame.folder, {
			targetSessionId: frame.session_id,
			resumeFile: frame.resume_file,
		})
		.then((rec) => {
			coordLink.send({
				kind: "rpc-ok",
				request_id,
				data: { session_id: rec.sessionId, channel_id: rec.channelId },
			});
		})
		.catch((err) => {
			coordLink.send({
				kind: "rpc-error",
				request_id,
				message: err instanceof Error ? err.message : String(err),
			});
		});
}

/** Composer send. Fire-and-forget: the reply lands as transcript entries, not
 *  as an RPC result, so there is no request_id on this frame. */
export function handleUserMessage(
	frame: Extract<ClientControlFrame, { kind: "user-message" }>,
	deps: AgentDeps,
): void {
	const rec = deps.sessionMgr.getBySessionId(frame.session_id);
	if (rec?.kind !== "agent") {
		log.warn("worker", "user_message_not_agent", { session_id: frame.session_id });
		return;
	}
	rec.agent.userMessage(frame.text);
}

export function handleAgentRespond(
	frame: Extract<ClientControlFrame, { kind: "agent-respond" }>,
	request_id: string,
	deps: AgentDeps,
): void {
	const agent = resolveAgent(frame.session_id, request_id, deps);
	if (!agent) return;
	// A stale or double-tapped prompt id is `accepted:false`, never an error —
	// the button may well have been clicked twice.
	const accepted = agent.respond(frame.prompt_id, frame.value, frame.cancelled);
	deps.coordLink.send({ kind: "rpc-ok", request_id, data: { accepted } });
}

export function handleAgentAbort(
	frame: Extract<ClientControlFrame, { kind: "omp-abort" }>,
	request_id: string,
	deps: AgentDeps,
): void {
	const agent = resolveAgent(frame.session_id, request_id, deps);
	if (!agent) return;
	agent.abort();
	deps.coordLink.send({ kind: "rpc-ok", request_id, data: { accepted: true } });
}

/** Transcript backfill. `cursor` carries the stringified before_seq; "0" or
 *  absent means the newest page. Coord Zod-parses each entry, so the payload
 *  ships the wire-shaped AgentEntry objects verbatim. */
export function handleGetAgentEntries(
	frame: Extract<ClientControlFrame, { kind: "get-omp-transcript-page" }>,
	request_id: string,
	deps: AgentDeps,
): void {
	const agent = resolveAgent(frame.session_id, request_id, deps);
	if (!agent) return;
	const parsed = Number(frame.cursor ?? "0");
	const beforeSeq = Number.isSafeInteger(parsed) && parsed > 0 ? parsed : 0;
	const page = agent.entriesPage(beforeSeq);
	deps.coordLink.send({ kind: "rpc-ok", request_id, data: page });
}
