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
import { isTerminalGeometry } from "@roost/shared/viewport";
import { FsmChannel } from "./fsm.ts";
import { canonicalSessionCwd } from "./util/path.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { _createWtermCore } from "./session-constants.ts";
import { createSbRing } from "./session-scrollback-ring.ts";
import { withAgentStatusEnvironment } from "./agent-status/environment.ts";
import { initAgentOscState } from "./terminal-stream-scan.ts";
import { resolveShellSpec } from "./shell-spec.ts";

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
	if (targetSessionId && this.getBySessionId(targetSessionId)) {
		throw new Error(`session ${targetSessionId} is already live`);
	}
	const spawnCols = cols ?? 80;
	const spawnRows = rows ?? 24;
	if (!isTerminalGeometry({ cols: spawnCols, rows: spawnRows })) {
		throw new Error("spawn geometry must be within 1..256 on both axes");
	}
	const channelId = this.nextChannelId();
	const sessionId = targetSessionId ?? asSessionId(randomUUID());
	const resolvedCwd = canonicalSessionCwd(cwd);
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
	const wtermCore = await _createWtermCore(spawnCols, spawnRows);
	if (wtermCore.getCols() !== spawnCols || wtermCore.getRows() !== spawnRows) {
		throw new Error("terminal core did not retain validated spawn geometry");
	}
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
		cell_emit: initCellEmitState(newTraceId(), randomUUID()),
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
		cols: spawnCols,
		rows: spawnRows,
	});

	try {
		record.childPid = await getMultiplexedPool().spawn({
			channelId,
			shellSpec,
			cols: spawnCols,
			rows: spawnRows,
			callbacks: this.muxCallbacks(channelId),
		});
	} catch (e) {
		this._dropChannelState(channelId);
		throw e;
	}
	this.lastAppliedSize.set(channelId, { cols: spawnCols, rows: spawnRows });


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
