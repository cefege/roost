// Resume a survivor-keeper session + respawn a dead PTY under the same sid.
// Split out of session-manager.ts (400-line cap); bodies byte-for-byte
// unchanged, called with a SessionManager `this`.

import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import type { SessionId, ChannelId } from "@roost/shared";
import { log, diag, signal } from "@roost/shared";
import { newTraceId } from "@roost/shared/trace";
import { initCellEmitState } from "@roost/shared/cell";
import { FsmChannel } from "./fsm.ts";
import { expandTilde } from "./util/path.ts";
import { withHistfile } from "./keeper/histfile.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { ALT_ENTER_SEQS, _scanAltModeTransitions } from "./terminal-stream-scan.ts";
import { _createWtermCore } from "./session-constants.ts";
import { createSbRing } from "./session-scrollback-ring.ts";
import { withAgentStatusEnvironment } from "./agent-status/environment.ts";
import { initAgentOscState } from "./terminal-stream-scan.ts";
import { existsSync } from "node:fs";
import { join } from "node:path";

/** Rebuild SessionRecord for a session whose keeper survived this
 * worker restart. Probes the mux pool; if the channel is alive in the
 * pool we re-attach callbacks. Returns true if resumed, false if no
 * surviving keeper. Does NOT re-emit `opened` — coord already
 * projected it before our restart. */
export async function resume(this: SessionManager, opts: {
	sessionId: SessionId;
	channelId: ChannelId;
	kind: "shell";
	cwd: string;
}): Promise<boolean> {
	if (this.sessions.has(opts.channelId)) return false;
	if (opts.channelId >= this._nextChannel)
		this._nextChannel = opts.channelId + 1;
	const fsm = new FsmChannel((from, to, event) =>
		this._onTransition(opts.sessionId, opts.channelId, from, to, event),
	);

	try {
		const pool = getMultiplexedPool();
		const live = await pool.listChannels();
		const liveChannel = live.find((c) => c.channelId === opts.channelId);
		if (!liveChannel) return false;
		pool.reattach(opts.channelId, this.muxCallbacks(opts.channelId));
		const wtermCore = await _createWtermCore(80, 24);
		// RC2: re-read the surviving channel's retained ring + head_seq from
		// the keeper so history survives this worker restart instead of being
		// zeroed. Resolves {headSeq:0, bytes:[]} on a keeper that pre-dates
		// GetHistory (timeout → graceful fallback to the old empty behavior).
		// Because the keeper's head_seq never reset (it outlived the worker),
		// the SPA's persisted lastSeq stays valid → clean delta on reconnect,
		// no seq-epoch reset. See [[project_scrollback_raw_ring_single_source]].
		let resumedBytes: Uint8Array = new Uint8Array(0);
		let resumedHeadSeq = 0;
		try {
			const hist = await pool.getHistory(opts.channelId);
			resumedBytes = hist.bytes;
			resumedHeadSeq = hist.headSeq;
		} catch (e) {
			log.warn("session-manager", "resume_history_failed", {
				channelId: opts.channelId,
				error: String(e),
			});
			signal("scrollback.history_lost", {
				sid: opts.sessionId,
				channel_id: opts.channelId,
				error: String(e),
				cooldownKey: opts.sessionId,
			});
		}
		// Derive the real alternate-screen state from retained terminal bytes.
		const resumedAlt =
			resumedBytes.length > 0
				? _scanAltModeTransitions(resumedBytes, false)
				: false;
		// Replay the ring into the headless core to reconstruct the current screen
		// for cell emission and fresh mounts.
		if (resumedBytes.length > 0) wtermCore.writeRaw(resumedBytes);
		// Restore an active alternate screen only when the retained stream shows
		// it. Replayed terminal replies belong to historical output and must not
		// be injected into live stdin.
		if (resumedAlt && !wtermCore.usingAltScreen())
			wtermCore.writeRaw(ALT_ENTER_SEQS[0]);
		wtermCore.getResponse();
		const record: SessionRecord = {
			sessionId: opts.sessionId,
			channelId: opts.channelId,
			socketPath: `mux:${opts.channelId}`,
			kind: opts.kind,
			cwd: opts.cwd,
			fsm,
			scrollback: createSbRing(resumedBytes),
			head_seq: resumedHeadSeq,
			alt_mode: resumedAlt,
			mode_carry: new Uint8Array(0),
			osc7_carry: new Uint8Array(0),
			...initAgentOscState(),
			wtermCore,
			session_trace_id: newTraceId(),
			cell_emit: initCellEmitState(),
			lastPtyOutMs: 0,
			spawnedAtMs: Date.now(),
			// Re-capture the child pid from listChannels so ports survive a worker
			// restart (reconcile adopts the keeper's live PTY, no re-spawn → the
			// childPid the port scan needs would otherwise be undefined → []).
			childPid: liveChannel.pid,
		};
		this.sessions.set(opts.channelId, record);
		this._startGitBranch(record);
		this._startPorts(record);
		// OMP bridge state reconnects independently of terminal byte replay.
		log.info("session-manager", "resumed", {
			sessionId: opts.sessionId,
			channelId: opts.channelId,
		});
		diag("session.attach", {
			sid: opts.sessionId,
			channel_id: opts.channelId,
			session_trace_id: record.session_trace_id,
			kind: opts.kind,
			resumed: true,
		});
		return true;
	} catch (e) {
		log.warn("session-manager", "resume_probe_failed", { error: String(e) });
		diag("session.resume_downgraded_respawn", {
			sid: opts.sessionId,
			error: String(e),
		});
		return false;
	}
}

/** Rebind an existing session_id to a fresh keeper PTY. Used by:
 *  (a) the boot-time auto-respawn loop when `resume()` returned false
 *      (the keeper PTY died with the Mac), and
 *  (b) future manual-restart paths.
 *  The session row in coord's DB keeps its identity (workspace
 *  assignment + sidebar position survive); only the underlying
 *  PTY channel is new. Emits a `respawned` event, not `opened`.
 *  No `closed` is emitted for the old PTY — the row is logically
 *  continuous, not torn down and re-created. */
export async function respawn(this: SessionManager, opts: {
	oldSessionId: SessionId;
	cwd: string;
	kind: "shell";
	cols?: number;
	rows?: number;
}): Promise<void> {
	// Tear down any leftover record without going through closedByKeeper
	// (which would emit a stray `closed`). Removing from this.sessions
	// first means the async Exit-frame round-trip from the keeper finds
	// no record and bails harmlessly.
	const existing = this.getBySessionId(opts.oldSessionId);
	if (existing) {
		this._dropChannelState(existing.channelId);
		getMultiplexedPool().kill(existing.channelId);
	}

	const channelId = this.nextChannelId();
	const resolvedCwd = expandTilde(opts.cwd);
	const cols = opts.cols ?? 80;
	const rows = opts.rows ?? 24;
	const fsm = new FsmChannel((from, to, event) =>
		this._onTransition(opts.oldSessionId, channelId, from, to, event),
	);
	const wtermCore = await _createWtermCore(cols, rows);

	const argv = [process.env.SHELL ?? "/bin/bash"];
	const env = withAgentStatusEnvironment(withHistfile(resolvedCwd), String(opts.oldSessionId));

	const record: SessionRecord = {
		sessionId: opts.oldSessionId,
		channelId,
		socketPath: `mux:${channelId}`,
		kind: opts.kind,
		cwd: resolvedCwd,
		fsm,
		scrollback: createSbRing(),
		head_seq: 0,
		// Stream-driven: a respawned TUI emits its own alternate-screen entry.
		alt_mode: false,
		mode_carry: new Uint8Array(0),
		osc7_carry: new Uint8Array(0),
		...initAgentOscState(),
		wtermCore,
		session_trace_id: newTraceId(),
		cell_emit: initCellEmitState(),
		lastPtyOutMs: 0,
		spawnedAtMs: Date.now(),
	};
	this.sessions.set(channelId, record);
	this._startGitBranch(record);
	this._startPorts(record);
	diag("session.spawn", {
		sid: opts.oldSessionId,
		channel_id: channelId,
		session_trace_id: record.session_trace_id,
		kind: opts.kind,
		cwd: resolvedCwd,
		cols,
		rows,
	});

	try {
		record.childPid = await getMultiplexedPool().spawn({
			channelId,
			cwd: resolvedCwd,
			argv,
			cols,
			rows,
			env,
			callbacks: this.muxCallbacks(channelId),
		});
	} catch (e) {
		this._dropChannelState(channelId);
		throw e;
	}

	this.emitEvent({
		kind: "respawned",
		session_id: opts.oldSessionId,
		new_channel: channelId,
		ts: Date.now(),
	});

	fsm.send({ kind: "attach" });
	log.info("session-manager", "respawned", {
		sessionId: opts.oldSessionId,
		channelId,
		cwd: resolvedCwd,
		kind: opts.kind,
	});
}
