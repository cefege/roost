// Fresh shell spawn + respawn-if-missing. Split out of
// session-manager.ts (400-line cap); called with a SessionManager `this`.

import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import type { SessionId } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { asSessionId } from "@roost/shared/wire";
import { randomUUID } from "node:crypto";
import { newTraceId } from "@roost/shared/trace";
import { initCellEmitState } from "@roost/shared/cell";
import { FsmChannel } from "./fsm.ts";
import { expandTilde } from "./util/path.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { _createWtermCore } from "./session-constants.ts";
import { createSbRing } from "./session-scrollback-ring.ts";
import { withAgentStatusEnvironment } from "./agent-status/environment.ts";
import { initAgentOscState } from "./terminal-stream-scan.ts";
import { resolveShellSpec } from "./shell-spec.ts";

export interface InitialViewportPreclaim {
	viewerId: string;
	clientSeq: bigint;
	cols: number;
	rows: number;
}
/** Spawn a plain shell session. Returns the channelId.
 *  targetSessionId: optional explicit session id to use instead of
 *  a fresh uuid. Coord's "respawn-if-missing" path uses this so the
 *  recreated PTY keeps the same sid as the DB row + the SPA URL. */
export async function spawnShell(
	this: SessionManager,
	cwd: string,
	cols?: number,
	rows?: number,
	targetSessionId?: SessionId,
	initialViewport?: InitialViewportPreclaim,
): Promise<SessionRecord> {
	if (targetSessionId && this.getBySessionId(targetSessionId)) {
		throw new Error(`session ${targetSessionId} is already live`);
	}
	if (
		initialViewport
		&& (
			initialViewport.cols !== cols
			|| initialViewport.rows !== rows
			|| initialViewport.clientSeq <= 0n
		)
	) {
		throw new Error("initial viewport preclaim does not match spawn dimensions");
	}
	const channelId = this.nextChannelId();
	const sessionId = targetSessionId ?? asSessionId(randomUUID());
	const resolvedCwd = expandTilde(cwd);
	const shellSpec = resolveShellSpec({
		cwd: resolvedCwd,
		sessionId: String(sessionId),
		envOverlay: withAgentStatusEnvironment({}, String(sessionId)),
	});
	const fsm = new FsmChannel((from, to, event) =>
		this._onTransition(sessionId, channelId, from, to, event),
	);

	const socketPath = `mux:${channelId}`;
	// Create wterm-core + register in sessions Map BEFORE awaiting
	// pool.spawn — between SpawnAck and sessions.set, the keeper can
	// already be emitting PtyOut frames (shell prints its prompt
	// immediately on exec). If the SessionRecord doesn't exist yet,
	// appendScrollback bails with -1 and the first prompt bytes are
	// dropped from BOTH the scrollback ring AND the upstream byte
	// stream — visible to user as "shell opened with no prompt".
	const wtermCore = await _createWtermCore(cols ?? 80, rows ?? 24);
	const record: SessionRecord = {
		sessionId,
		channelId,
		socketPath,
		kind: "shell",
		cwd: resolvedCwd,
		shellSpec,
		fsm,
		scrollback: createSbRing(),
		head_seq: 0,
		alt_mode: false,
		mode_carry: new Uint8Array(0),
		osc7_carry: new Uint8Array(0),
		query_carry: new Uint8Array(0),
		...initAgentOscState(),
		wtermCore,
		session_trace_id: newTraceId(),
		cell_emit: initCellEmitState(newTraceId()),
		lastPtyOutMs: 0,
		sb_origin_pin: null,
		spawnedAtMs: Date.now(),
	};
	this.sessions.set(channelId, record);
	this.channelResizeSeq.set(channelId, 0);
	diag("session.spawn", {
		sid: sessionId,
		channel_id: channelId,
		session_trace_id: record.session_trace_id,
		kind: "shell",
		cwd: resolvedCwd,
		cols: cols ?? 80,
		rows: rows ?? 24,
	});

	try {
		record.childPid = await getMultiplexedPool().spawn({
			channelId,
			shellSpec,
			cols: cols ?? 80,
			rows: rows ?? 24,
			callbacks: this.muxCallbacks(channelId),
		});
	} catch (e) {
		this._dropChannelState(channelId);
		throw e;
	}
	// The keeper created the PTY at exactly this size, so it is PROVEN applied
	// geometry — not a guess. Recording it here is what lets the first claim at the
	// same size take the locally-proven no-resize path instead of installing a
	// capture and reconciling a resize that changes nothing.
	this.lastAppliedSize.set(channelId, { cols: cols ?? 80, rows: rows ?? 24 });

	// The PTY/core already started at this exact size. Install the authenticated
	// viewer directly instead of routing through claimViewport, which would
	// schedule a redundant resize/rebuild and could emit cells before `opened`.
	if (initialViewport) {
		this.viewportClaims.set(channelId, new Map([[
			initialViewport.viewerId,
			{
				cols: initialViewport.cols,
				rows: initialViewport.rows,
				lastMs: Date.now(),
				clientSeq: initialViewport.clientSeq,
			},
		]]));
		this.lastAppliedSize.set(channelId, {
			cols: initialViewport.cols,
			rows: initialViewport.rows,
		});
	}

	this.emitEvent({
		kind: "opened",
		session_id: sessionId,
		worker_fp: this.workerFp,
		channel: channelId,
		session_kind: "shell",
		cwd: resolvedCwd,
		ts: Date.now(),
	});

	// The durable opened frame is the chronology fence. The first viewport-only
	// full follows it on the worker link, and rpc-ok follows the full.
	if (initialViewport) this.emitCellSnapshot(channelId);

	// The keeper PTY is the initial attachment, so every natural close
	// satisfies the lifecycle invariant.
	fsm.send({ kind: "attach" });
	this._startGitBranch(record);
	this._startPorts(record);
	log.info("session-manager", "shell spawned", {
		sessionId,
		channelId,
		cwd: resolvedCwd,
	});
	return record;
}



/** Coord respawn-if-missing handler. Idempotent: if the worker already has the
 * session live (survivor keeper resumed it), this returns the existing record.
 * Otherwise it recreates the terminal under the same session id. */
export async function respawnIfMissing(
	this: SessionManager,
	sessionId: SessionId,
	cwd: string,
	cols: number,
	rows: number,
): Promise<SessionRecord> {
	const existing = this.getBySessionId(sessionId);
	if (existing) return existing;
	log.info("session-manager", "respawn_if_missing_spawning", {
		sessionId,
		cwd,
		cols,
		rows,
	});
	diag("session.respawn_if_missing", {
		sid: sessionId,
		kind: "shell",
		cwd,
		cols,
		rows,
	});
	return this.spawnShell(cwd, cols, rows, sessionId);
}
