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
import { isTerminalGeometry } from "@roost/shared/viewport";
import { randomUUID } from "node:crypto";
import { FsmChannel } from "./fsm.ts";
import { canonicalSessionCwd } from "./util/path.ts";
import {
	getMultiplexedPool,
	type KeeperHistoryRecords,
} from "./keeper/multiplexed-client.ts";
import { ALT_ENTER_SEQS, _scanAltModeTransitions, initAgentOscState } from "./terminal-stream-scan.ts";
import { _createWtermCore, RESUME_STAGE_CAP_BYTES } from "./session-constants.ts";
import { drainCoreReplies } from "./terminal-query-reply.ts";
import { appendToRing, createSbRing, readRing } from "./session-scrollback-ring.ts";
import { skipOrphanSequencePrefix } from "./terminal-replay-align.ts";
import { withAgentStatusEnvironment } from "./agent-status/environment.ts";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { resolveShellSpec, type ShellSpec } from "./shell-spec.ts";
import { KeeperFeature } from "./keeper/protocol.ts";
import {
	flushResumeEvents,
	stageResumeCallbacks,
	type PendingResumeEvent,
	type ResumeStageState,
} from "./session-resume-events.ts";

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
	if (opts.channelId >= this._nextChannel) this._nextChannel = opts.channelId + 1;
	const fsm = new FsmChannel((from, to, event) =>
		this._onTransition(opts.sessionId, opts.channelId, from, to, event),
	);

	const pool = getMultiplexedPool();
	const liveCallbacks = this.muxCallbacks(opts.channelId);
	const pendingEvents: PendingResumeEvent[] = [];
	const stage: ResumeStageState = { overflowed: false };
	try {
		const live = await pool.listChannels();
		const liveChannel = live.find((c) => c.channelId === opts.channelId);
		if (!liveChannel) return false;
		// Reattach must precede the history request so the keeper can establish
		// its ordered boundary. The SessionRecord cannot exist until that history
		// has rebuilt a core, so stage post-boundary events instead of feeding
		// them into emitUpstreamChunk, which correctly rejects unknown channels.
		pool.reattach(
			opts.channelId,
			stageResumeCallbacks(liveCallbacks, pendingEvents, stage),
		);
		let orderedHistory: KeeperHistoryRecords | null = null;
		let legacyBytes: Uint8Array = new Uint8Array(0);
		let resumedHeadSeq = 0;
		let historyEvicted = false;
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
			// Drain-only compatibility for the deployed pre-capability keeper; new keepers take the branch above.
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
		const terminalState = await pool.getTerminalState(opts.channelId);
		if (!terminalState || !isTerminalGeometry({ cols: terminalState.cols, rows: terminalState.rows })) {
			throw new Error("keeper did not report valid terminal geometry for adoption");
		}
		const baseCols = orderedHistory?.baseCols ?? terminalState.cols;
		const baseRows = orderedHistory?.baseRows ?? terminalState.rows;
		if (!isTerminalGeometry({ cols: baseCols, rows: baseRows })) {
			throw new Error("keeper history reported invalid base terminal geometry");
		}
		const wtermCore = await _createWtermCore(baseCols, baseRows);
		if (wtermCore.getCols() !== baseCols || wtermCore.getRows() !== baseRows) {
			throw new Error("terminal core did not retain keeper history base geometry");
		}
		const resumedScrollback = createSbRing();
		// The keeper's history window opens at whatever byte its own ring last
		// evicted over (keeper/keeper-frame-handler.ts orderedHistory() reads
		// outRing from offset 0), so under eviction the first replayed byte can be
		// the tail of a sequence whose `ESC [` was overwritten in place. This core's
		// parser is COLD, so that remnant prints as LITERAL text (`32m`) and then
		// sticks: nothing downstream re-parses, and a TUI's cursor-addressed partial
		// repaint never revisits a cell it believes it already painted. Unlike a
		// resize boundary there is nothing to rewind onto — the bytes that would
		// give the remnant its parser context were overwritten at the keeper and are
		// gone — so dropping the orphan prefix is the only sound repair.
		//
		// Only when the keeper actually evicted: head_seq counts every byte the pty
		// ever produced, so `head_seq === retained` proves the window still starts at
		// the true start of the stream, which is token-aligned by construction and
		// whose leading plain text is real history worth keeping.
		//
		// The RING keeps the untrimmed bytes either way: session-resize-capture.ts
		// derives `retainedStart = head_seq - ringLength(scrollback)`, so shortening
		// the ring without moving head_seq would skew every later boundary offset by
		// exactly the dropped count.
		if (orderedHistory) {
			const retainedTotal = orderedHistory.records.reduce(
				(total, historyRecord) =>
					historyRecord.kind === "output" ? total + historyRecord.bytes.byteLength : total,
				0,
			);
			historyEvicted = resumedHeadSeq > retainedTotal;
			// True only for the cold core's first write, and only under eviction.
			// Every later record continues that same now-warm parser stream and must
			// be replayed verbatim.
			let dropOrphanPrefix = historyEvicted;
			for (const historyRecord of orderedHistory.records) {
				if (historyRecord.kind === "output") {
					appendToRing(resumedScrollback, historyRecord.bytes);
					wtermCore.writeRaw(
						dropOrphanPrefix
							? historyRecord.bytes.subarray(skipOrphanSequencePrefix(historyRecord.bytes))
							: historyRecord.bytes,
					);
					dropOrphanPrefix = false;
				} else {
					if (!isTerminalGeometry({ cols: historyRecord.cols, rows: historyRecord.rows })) {
						throw new Error("keeper history contains invalid resize geometry");
					}
					wtermCore.resize(historyRecord.cols, historyRecord.rows);
					if (
						wtermCore.getCols() !== historyRecord.cols
						|| wtermCore.getRows() !== historyRecord.rows
					) {
						throw new Error("terminal core did not retain keeper history resize geometry");
					}
			}
			}
		} else {
			historyEvicted = resumedHeadSeq > legacyBytes.length;
			if (legacyBytes.length > 0) {
				appendToRing(resumedScrollback, legacyBytes);
				wtermCore.writeRaw(
					historyEvicted
						? legacyBytes.subarray(skipOrphanSequencePrefix(legacyBytes))
						: legacyBytes,
				);
			}
		}
		if (
			wtermCore.getCols() !== terminalState.cols
			|| wtermCore.getRows() !== terminalState.rows
		) {
			throw new Error("ordered keeper history did not converge to reported terminal geometry");
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
			cell_emit: initCellEmitState(newTraceId(), randomUUID()),
			lastPtyOutMs: 0,
			sb_origin_pin: null,
			spawnedAtMs: Date.now(),
			// Re-capture the child pid from listChannels so ports survive a worker
			// restart (reconcile adopts the keeper's live PTY, no re-spawn → the
			// childPid the port scan needs would otherwise be undefined → []).
			childPid: liveChannel.pid,
		};
		this.sessions.set(opts.channelId, record);
		this.channelResizeSeq.set(opts.channelId, terminalState.highestResizeSeq);
		this.lastAppliedSize.set(opts.channelId, { cols: record.wtermCore.getCols(), rows: record.wtermCore.getRows() });
		// Adoption must be atomic in stream terms: if staging overflowed while
		// the core was being rebuilt, bytes were already dropped and replaying
		// the remainder would splice a hole into a live parser. Throwing HERE
		// (not inside the keeper callback) lands in this function's own catch,
		// which kills the channel and respawns it fresh under the same sid.
		if (stage.overflowed) {
			throw new Error(`resume staged output exceeded ${RESUME_STAGE_CAP_BYTES} bytes; adopting would splice a byte gap`);
		}
		// Swap to the ordinary callbacks synchronously, then replay everything
		// delivered after the keeper's history boundary. JS cannot interleave a
		// socket callback between this swap and the synchronous flush.
		pool.reattach(opts.channelId, liveCallbacks);
		flushResumeEvents(liveCallbacks, pendingEvents);
		// The survivor was found and its real exit was delivered. Report the
		// reconciliation as handled so boot-reconcile does not respawn it.
		if (!this.sessions.has(opts.channelId)) return true;
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
		// Adoption failed AFTER reattach: the channel is live+attached but has
		// NO SessionRecord, so its PtyOut frames land on emit_no_session and a
		// chatty orphan crosses KEEPER_DEGRADED_THRESHOLD → onKeeperDegraded
		// SIGTERMs every healthy session. Kill via the pool's own primitive,
		// mark recently-closed so in-flight/tail frames drop through the tail
		// gate, then replay staged events through live callbacks; finally emit
		// the closed tombstone a mid-resume death produces and report failure
		// so boot reconciliation respawns under the same sid.
		pool.kill(opts.channelId);
		this.markRecentlyClosed(opts.channelId);
		pool.reattach(opts.channelId, liveCallbacks);
		flushResumeEvents(liveCallbacks, pendingEvents);
		this.emitClosedTombstone(opts.sessionId);
		log.warn("session-manager", "resume_probe_failed", { error: String(e) });
		diag("session.resume_downgraded_respawn", { sid: opts.sessionId, error: String(e) });
		return false;
	}
}

/** Rebind an existing session_id to a fresh keeper PTY: the boot-time
 *  auto-respawn loop after `resume()` returned false, plus future manual
 *  restarts. Coord's row keeps its identity (workspace + sidebar); only the PTY channel is new.
 *  Emits `respawned`, not `opened`/`closed` — logically continuous, not recreated. */
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
	const existingSize = existing ? { cols: existing.wtermCore.getCols(), rows: existing.wtermCore.getRows() } : null;
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
	const cols = opts.cols ?? existingSize?.cols ?? 80;
	const rows = opts.rows ?? existingSize?.rows ?? 24;
	if (!isTerminalGeometry({ cols: cols, rows: rows })) {
		throw new Error("respawn geometry must be within 1..256 on both axes");
	}
	const fsm = new FsmChannel((from, to, event) =>
		this._onTransition(opts.oldSessionId, channelId, from, to, event),
	);
	const wtermCore = await _createWtermCore(cols, rows);
	if (wtermCore.getCols() !== cols || wtermCore.getRows() !== rows) {
		throw new Error("terminal core did not retain validated respawn geometry");
	}

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
		cell_emit: initCellEmitState(newTraceId(), randomUUID()),
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

	// The keeper created the PTY at exactly cols×rows — proven applied geometry a
	// later coordinator stream reuses without a no-op resize or core replacement.
	this.channelResizeSeq.set(channelId, 0);
	this.lastAppliedSize.set(channelId, { cols, rows });

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
