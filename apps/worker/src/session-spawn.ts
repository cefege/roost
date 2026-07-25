// Fresh shell/claude spawn + respawn-if-missing. Split out of
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
import { buildHooksSettings } from "./claude/hooks.ts";
import { _createWtermCore, HOOK_CMD } from "./session-constants.ts";

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
		scrollback: new Uint8Array(0),
		head_seq: 0,
		alt_mode: false,
		mode_carry: new Uint8Array(0),
		osc7_carry: new Uint8Array(0),
		wtermCore,
		session_trace_id: newTraceId(),
		cell_emit: initCellEmitState(),
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

	// FSM symmetry with spawnClaude: fire attach so the R5.3 invariant
	// ("cannot reach closed without passing through attached") holds
	// for shell sessions too. Without it, every shell session that
	// exits naturally goes spawned → closed directly, leaving
	// hasEverAttached=false and breaking any future consumer that
	// filters on it.
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

/** Spawn a claude agent session. Returns the channelId.
 *  targetSessionId: optional explicit session id (respawn-if-missing). */
export async function spawnClaude(
	this: SessionManager,
	cwd: string,
	initialMode: string,
	cols?: number,
	rows?: number,
	targetSessionId?: SessionId,
): Promise<SessionRecord> {
	const channelId = this.nextChannelId();
	const sessionId = targetSessionId ?? asSessionId(randomUUID());
	const claudeBin = "/opt/homebrew/bin/claude";
	const resolvedCwd = expandTilde(cwd);

	// Hooks ride the PTY claude — the process the user actually drives.
	// (The old shadow ClaudeBridge spawned a SECOND headless claude that
	// never received input, so its hooks never fired → session.agent stayed
	// null fleet-wide. Deleted 2026-07-04.)
	const argv = [
		claudeBin,
		"--input-format=stream-json",
		"--output-format=stream-json",
		"--verbose",
		"--include-hook-events",
		"--allow-dangerously-skip-permissions",
		"--permission-mode",
		initialMode,
		"--settings",
		buildHooksSettings(this.hookSocketPath, HOOK_CMD),
	];

	const fsm = new FsmChannel((from, to, event) =>
		this._onTransition(sessionId, channelId, from, to, event),
	);

	const socketPath = `mux:${channelId}`;
	// Same ordering rule as spawnShell: register the SessionRecord
	// BEFORE pool.spawn so the first PtyOut bytes don't get dropped.
	const wtermCore = await _createWtermCore(cols ?? 80, rows ?? 24);
	// alt_mode starts FALSE and is driven by _scanAltModeTransitions on the
	// real byte stream (handleBytes ~:395). Claude Code is MAIN-SCREEN (it
	// never emits ESC[?1049h in normal use → usingAltScreen()=false), so it
	// MUST start non-alt to keep its scrollback (cell emitter ships sb_append,
	// getScrollbackSince serves the raw ring). If a claude subview DOES enter
	// alt-screen the scan flips alt_mode=true and the c03d62d0 rebuild
	// carve-out fires. Hardcoding alt_mode:true here was the "claude has no
	// scrollback in cell mode / lost on reload" bug (2026-06-22b).
	const record: SessionRecord = {
		sessionId,
		channelId,
		socketPath,
		kind: "claude",
		cwd: resolvedCwd,
		fsm,
		scrollback: new Uint8Array(0),
		head_seq: 0,
		alt_mode: false,
		mode_carry: new Uint8Array(0),
		osc7_carry: new Uint8Array(0),
		wtermCore,
		session_trace_id: newTraceId(),
		cell_emit: initCellEmitState(),
		spawnedAtMs: Date.now(),
	};
	this.sessions.set(channelId, record);
	diag("session.spawn", {
		sid: sessionId,
		channel_id: channelId,
		session_trace_id: record.session_trace_id,
		kind: "claude",
		cwd: resolvedCwd,
		cols: cols ?? 80,
		rows: rows ?? 24,
	});

	try {
		record.childPid = await getMultiplexedPool().spawn({
			channelId,
			cwd: resolvedCwd,
			argv,
			cols: cols ?? 80,
			rows: rows ?? 24,
			// cli/hook.ts reads both: the socket to POST to + the session id to
			// tag with.
			env: {
				ROOST_HOOK_SOCKET: this.hookSocketPath,
				ROOST_SURFACE_ID: sessionId,
			},
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
		session_kind: "claude",
		cwd: resolvedCwd,
		ts: Date.now(),
	});

	// FSM lives in `spawned` until `attach` advances it. The keeper PTY
	// IS the attachment from the worker's perspective — without firing
	// attach first, `agent-started` would be rejected by the transition
	// table (TRANSITIONS.spawned has no `agent-started` entry) and every
	// subsequent agent-running/needs-input/idle event would silently
	// no-op. Fire attach explicitly before the agent transition.
	fsm.send({ kind: "attach" });
	fsm.send({ kind: "agent-started" });
	this._startGitBranch(record);
	this._startPorts(record);
	log.info("session-manager", "claude spawned", {
		sessionId,
		channelId,
		cwd: resolvedCwd,
	});
	return record;
}


/** Coord respawn-if-missing handler. Idempotent: if the worker
 *  already has the session live (survivor keeper resumed it), this
 *  is a no-op and returns the existing record. Else spawns a fresh
 *  PTY with the requested cwd/kind/dims AND the same sessionId — so
 *  the coord DB row, the SPA URL, and the new PTY all share a sid. */
export async function respawnIfMissing(
	this: SessionManager,
	sessionId: SessionId,
	kind: "shell" | "claude",
	cwd: string,
	cols: number,
	rows: number,
): Promise<SessionRecord> {
	const existing = this.getBySessionId(sessionId);
	if (existing) return existing;
	log.info("session-manager", "respawn_if_missing_spawning", {
		sessionId,
		kind,
		cwd,
		cols,
		rows,
	});
	diag("session.respawn_if_missing", {
		sid: sessionId,
		kind,
		cwd,
		cols,
		rows,
	});
	if (kind === "shell") return this.spawnShell(cwd, cols, rows, sessionId);
	return this.spawnClaude(cwd, "default", cols, rows, sessionId);
}
