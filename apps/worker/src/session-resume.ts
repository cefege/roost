// Resume a survivor-keeper session + respawn a dead PTY under the same sid.
// Split out of session-manager.ts (400-line cap); called with a
// SessionManager `this`.

import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import type { SessionId, ChannelId } from "@roost/shared/wire";
import { diag, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { newTraceId } from "@roost/shared/trace";
import { initCellEmitState } from "@roost/shared/cell";
import { FsmChannel } from "./fsm.ts";
import { canonicalSessionCwd } from "./util/path.ts";
import {
	getMultiplexedPool,
	type KeeperHistoryRecords,
} from "./keeper/multiplexed-client.ts";
import { ALT_ENTER_SEQS, _scanAltModeTransitions } from "./terminal-stream-scan.ts";
import { _createWtermCore } from "./session-constants.ts";
import { drainCoreReplies } from "./terminal-query-reply.ts";
import { appendToRing, createSbRing, readRing } from "./session-scrollback-ring.ts";
import { withAgentStatusEnvironment } from "./agent-status/environment.ts";
import { initAgentOscState } from "./terminal-stream-scan.ts";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { resolveShellSpec } from "./shell-spec.ts";
import type { ShellSpec } from "./shell-spec.ts";
import { KeeperFeature } from "./keeper/protocol.ts";

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
	shellSpec: ShellSpec;
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
		let orderedHistory: KeeperHistoryRecords | null = null;
		let legacyBytes: Uint8Array = new Uint8Array(0);
		let resumedHeadSeq = 0;
		let historyError: unknown;
		const orderedHistorySupported = pool.supportsKeeperFeature(
			KeeperFeature.OrderedHistory,
		);
		if (orderedHistorySupported) {
			try {
				orderedHistory = await pool.getHistoryRecords(opts.channelId);
				resumedHeadSeq = orderedHistory.headSeq;
			} catch (error) {
				historyError = error;
			}
		} else {
			// Drain-only compatibility for the currently deployed pre-capability
			// keeper. New keepers always take the ordered branch above.
			try {
				const legacy = await pool.getHistory(opts.channelId);
				legacyBytes = legacy.bytes;
				resumedHeadSeq = legacy.headSeq;
			} catch (error) {
				historyError = error;
			}
		}
		if (historyError) {
			log.warn("session-manager", "resume_history_failed", {
				channelId: opts.channelId,
				error: String(historyError),
			});
			signal("scrollback.history_lost", {
				sid: opts.sessionId,
				channel_id: opts.channelId,
				error: String(historyError),
				cooldownKey: opts.sessionId,
			});
			// A keeper that advertised ordered history must satisfy that
			// contract. Adopting it with an empty 80x24 core would replay bytes
			// under the wrong geometry, so force the normal respawn path.
			if (orderedHistorySupported) throw historyError;
		}
		const wtermCore = await _createWtermCore(
			orderedHistory?.baseCols ?? 80,
			orderedHistory?.baseRows ?? 24,
		);
		const resumedScrollback = createSbRing();
		if (orderedHistory) {
			for (const historyRecord of orderedHistory.records) {
				if (historyRecord.kind === "output") {
					appendToRing(resumedScrollback, historyRecord.bytes);
					wtermCore.writeRaw(historyRecord.bytes);
				} else {
					wtermCore.resize(historyRecord.cols, historyRecord.rows);
				}
			}
		} else if (legacyBytes.length > 0) {
			appendToRing(resumedScrollback, legacyBytes);
			wtermCore.writeRaw(legacyBytes);
		}
		const resumedBytes = readRing(resumedScrollback);
		const resumedAlt = resumedBytes.length > 0
			? _scanAltModeTransitions(resumedBytes, false)
			: false;
		// Restore an active alternate screen only when the retained stream shows
		// it. Replayed terminal replies belong to historical output and must not
		// be injected into live stdin.
		if (resumedAlt && !wtermCore.usingAltScreen())
			wtermCore.writeRaw(ALT_ENTER_SEQS[0]);
		drainCoreReplies(wtermCore);
		const record: SessionRecord = {
			sessionId: opts.sessionId,
			channelId: opts.channelId,
			socketPath: `mux:${opts.channelId}`,
			kind: opts.kind,
			cwd: opts.cwd,
			shellSpec: opts.shellSpec,
			fsm,
			scrollback: resumedScrollback,
			head_seq: resumedHeadSeq,
			alt_mode: resumedAlt,
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
			// Re-capture the child pid from listChannels so ports survive a worker
			// restart (reconcile adopts the keeper's live PTY, no re-spawn → the
			// childPid the port scan needs would otherwise be undefined → []).
			childPid: liveChannel.pid,
		};
		this.sessions.set(opts.channelId, record);
		this.channelResizeSeq.set(
			opts.channelId,
			orderedHistory?.records.reduce(
				(latest, historyRecord) =>
					historyRecord.kind === "resize" ? Math.max(latest, historyRecord.seq) : latest,
				0,
			) ?? 0,
		);
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
	shellSpec?: ShellSpec;
}): Promise<void> {
	// Tear down any leftover record without going through closedByKeeper
	// (which would emit a stray `closed`). Removing from this.sessions
	// first means the async Exit-frame round-trip from the keeper finds
	// no record and bails harmlessly.
	const existing = this.getBySessionId(opts.oldSessionId);
	const requestedCwd = canonicalSessionCwd(opts.cwd);
	const shellSpec = existing?.shellSpec ?? opts.shellSpec ?? resolveShellSpec({
		cwd: requestedCwd,
		sessionId: String(opts.oldSessionId),
		envOverlay: withAgentStatusEnvironment({}, String(opts.oldSessionId)),
	});
	if (existing) {
		this._dropChannelState(existing.channelId);
		getMultiplexedPool().kill(existing.channelId);
	}

	const channelId = this.nextChannelId();
	const resolvedCwd = shellSpec.cwd;
	const cols = opts.cols ?? 80;
	const rows = opts.rows ?? 24;
	const fsm = new FsmChannel((from, to, event) =>
		this._onTransition(opts.oldSessionId, channelId, from, to, event),
	);
	const wtermCore = await _createWtermCore(cols, rows);

	const record: SessionRecord = {
		sessionId: opts.oldSessionId,
		channelId,
		socketPath: `mux:${channelId}`,
		kind: opts.kind,
		cwd: resolvedCwd,
		shellSpec,
		fsm,
		scrollback: createSbRing(),
		head_seq: 0,
		// Stream-driven: a respawned TUI emits its own alternate-screen entry.
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
			shellSpec,
			cols,
			rows,
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
