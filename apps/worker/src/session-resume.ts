// Session adoption rebuilds a worker record around a PTY whose keeper survived restart.
// It replays retained output and geometry before exposing the channel to live callbacks.
// SessionManager uses a failed adoption as the signal to respawn the logical session.

import {
	isSessionLifecycleDurabilityError,
	type SessionManager,
} from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import type { LifecycleReservation } from "./event-sink.ts";
import type { SessionId, ChannelId } from "@roost/shared/wire";
import { diag, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { newTraceId } from "@roost/shared/trace";
import { initCellEmitState } from "@roost/shared/cell";
import { isTerminalGeometry } from "@roost/shared/viewport";
import { randomUUID } from "node:crypto";
import { FsmChannel } from "./fsm.ts";
import {
	getMultiplexedPool,
	type KeeperHistoryRecords,
} from "./keeper/multiplexed-client.ts";
import { ALT_ENTER_SEQS, _scanAltModeTransitions, initAgentOscState } from "./terminal-stream-scan.ts";
import { _createWtermCore, RESUME_STAGE_CAP_BYTES } from "./session-constants.ts";
import { drainCoreReplies } from "./terminal-query-reply.ts";
import { appendToRing, createSbRing, readRing } from "./session-scrollback-ring.ts";
import { skipOrphanSequencePrefix } from "./terminal-replay-align.ts";
import type { ShellSpec } from "./shell-spec.ts";
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
export async function resume(
	this: SessionManager,
	opts: {
		sessionId: SessionId;
		channelId: ChannelId;
		kind: "shell";
		cwd: string;
		shellSpec: ShellSpec;
	},
	closeReservation: LifecycleReservation,
): Promise<boolean> {
	if (this.sessions.has(opts.channelId)) {
		this.releaseLifecycleEvent(closeReservation);
		return false;
	}
	if (opts.channelId >= this._nextChannel) this._nextChannel = opts.channelId + 1;
	const fsm = new FsmChannel((from, to, event) =>
		this._onTransition(opts.sessionId, opts.channelId, from, to, event),
	);

	const pool = getMultiplexedPool();
	const liveCallbacks = this.muxCallbacks(opts.channelId);
	const pendingEvents: PendingResumeEvent[] = [];
	const stage: ResumeStageState = { overflowed: false };
	let recordInstalled = false;
	try {
		const live = await pool.listChannels();
		const liveChannel = live.find((c) => c.channelId === opts.channelId);
		if (!liveChannel) {
			this.releaseLifecycleEvent(closeReservation);
			return false;
		}
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
			closeReservation,
			// Re-capture the child pid from listChannels so ports survive a worker
			// restart (reconcile adopts the keeper's live PTY, no re-spawn → the
			// childPid the port scan needs would otherwise be undefined → []).
			childPid: liveChannel.pid,
		};
		this.sessions.set(opts.channelId, record);
		recordInstalled = true;
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
		this.holdLifecycleEvent(closeReservation);
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
		if (isSessionLifecycleDurabilityError(e)) throw e;
		// Adoption failed after the possible reattach. Kill before releasing any
		// lifecycle capacity so no orphan can race beyond its durable close.
		pool.kill(opts.channelId);
		this.markRecentlyClosed(opts.channelId);
		pool.reattach(opts.channelId, liveCallbacks);
		flushResumeEvents(liveCallbacks, pendingEvents);
		const installed = this.sessions.get(opts.channelId);
		if (installed) {
			this.closedByKeeper(opts.channelId, null);
		} else if (recordInstalled) {
			// A staged Exit already consumed the record's held reservation.
		} else {
			this.emitClosedTombstone(opts.sessionId, closeReservation);
		}
		log.warn("session-manager", "resume_probe_failed", { error: String(e) });
		diag("session.resume_downgraded_respawn", { sid: opts.sessionId, error: String(e) });
		return false;
	}
}
