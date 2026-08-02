// Fresh shell spawn + respawn-if-missing. Split out of
// session-manager.ts (400-line cap); called with a SessionManager `this`.

import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import type { SessionId } from "@roost/shared";
import { asSessionId, log, diag } from "@roost/shared";
import { randomUUID } from "node:crypto";
import { newTraceId } from "@roost/shared/trace";
import { initCellEmitState } from "@roost/shared/cell";
import { FsmChannel } from "./fsm.ts";
import { expandTilde } from "./util/path.ts";
import { withHistfile } from "./keeper/histfile.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { _createWtermCore } from "./session-constants.ts";
import { createSbRing } from "./session-scrollback-ring.ts";

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
): Promise<SessionRecord> {
	const channelId = this.nextChannelId();
	const sessionId = targetSessionId ?? asSessionId(randomUUID());
	const shell = process.env.SHELL ?? "/bin/bash";
	const resolvedCwd = expandTilde(cwd);
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
		fsm,
		scrollback: createSbRing(),
		head_seq: 0,
		alt_mode: false,
		mode_carry: new Uint8Array(0),
		osc7_carry: new Uint8Array(0),
		wtermCore,
		session_trace_id: newTraceId(),
		cell_emit: initCellEmitState(),
		lastPtyOutMs: 0,
		spawnedAtMs: Date.now(),
	};
	this.sessions.set(channelId, record);
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
			cwd: resolvedCwd,
			argv: [shell],
			cols: cols ?? 80,
			rows: rows ?? 24,
			env: withHistfile(resolvedCwd),
			callbacks: this.muxCallbacks(channelId),
		});
	} catch (e) {
		this._dropChannelState(channelId);
		throw e;
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
