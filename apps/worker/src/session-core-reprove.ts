// Failure-path repair for a terminal stream whose core is fail-closed
// (TerminalStreamState.coreValid === false): rebuild THAT still-live channel's
// core in place from the keeper's ordered history so the stream can emit again.
// session-terminal-txn.ts is the only caller, immediately before it would
// refuse a stream desire. This is NOT the removed rebuild-from-ring-on-every-
// resize path docs/FAILURE-INDEX.md condemns: a provable resize still resizes
// the core it already owns, and nothing here runs while coreValid holds.

import { initCellEmitState } from "@roost/shared/cell";
import { signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { newTraceId } from "@roost/shared/trace";
import { isTerminalGeometry } from "@roost/shared/viewport";
import type { TerminalCore } from "@wterm/core";
import {
	getMultiplexedPool,
	type KeeperHistoryRecords,
	type KeeperTerminalState,
} from "./keeper/multiplexed-client.ts";
import { KeeperFeature } from "./keeper/protocol.ts";
import type { SessionManager } from "./session-manager.ts";
import {
	appendToRing,
	createSbRing,
	readRing,
	type SbRing,
} from "./session-scrollback-ring.ts";
import type { TerminalStreamState } from "./session-terminal-state.ts";
import { isTerminalCoreCapacityError } from "./terminal-core-capacity.ts";
import { drainCoreReplies } from "./terminal-query-reply.ts";
import { skipOrphanSequencePrefix } from "./terminal-replay-align.ts";
import { ALT_ENTER_SEQS } from "./terminal-stream-scan.ts";

export interface TerminalCoreReproofResult {
	ok: boolean;
	/** Present when ok === false. */
	reason?: string;
	/** A newer desire, a closed session or an expired budget: the caller
	 *  reports retryable_pre_write so the one classify() retry re-drives. */
	retryable?: boolean;
}

/** Legacy keeper history, for a survivor that predates ordered records. */
interface LegacyKeeperHistory {
	headSeq: number;
	bytes: Uint8Array;
}

interface HistoryReplay {
	ring: SbRing;
	/** Byte cursor at the end of `ring`; the rebuilt window's own head. */
	headSeq: number;
	replayedBytes: number;
	tailBytes: number;
	historyEvicted: boolean;
}

const SUPERSEDED = "terminal stream was superseded during core re-proof";

export async function reproveTerminalCore(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
): Promise<TerminalCoreReproofResult> {
	const rec = mgr.sessions.get(channelId);
	if (!rec) return refuse(channelId, state, "session is not live", true);

	const pool = getMultiplexedPool();
	const ordered = pool.supportsKeeperFeature(KeeperFeature.OrderedHistory);
	let history: KeeperHistoryRecords | null = null;
	let legacy: LegacyKeeperHistory | null = null;
	try {
		if (ordered) history = await pool.getHistoryRecords(channelId);
		else legacy = await pool.getHistory(channelId);
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		return refuse(channelId, state, `keeper history unavailable: ${message}`, false);
	}
	let terminalState: KeeperTerminalState | null;
	try {
		terminalState = await pool.getTerminalState(channelId);
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		return refuse(channelId, state, `keeper terminal state unavailable: ${message}`, false);
	}
	if (
		!terminalState
		|| !isTerminalGeometry({ cols: terminalState.cols, rows: terminalState.rows })
	) {
		return refuse(channelId, state, "keeper did not report terminal geometry", false);
	}
	if (mgr.sessions.get(channelId) !== rec || mgr.terminalStreams.get(channelId) !== state) {
		return refuse(channelId, state, SUPERSEDED, true);
	}

	const baseCols = history?.baseCols ?? terminalState.cols;
	const baseRows = history?.baseRows ?? terminalState.rows;
	if (!isTerminalGeometry({ cols: baseCols, rows: baseRows })) {
		return refuse(channelId, state, "keeper history reported invalid base geometry", false);
	}
	let lease;
	try {
		lease = mgr.reserveTerminalCore("replacement");
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		return refuse(
			channelId,
			state,
			`terminal core reservation refused: ${message}`,
			isTerminalCoreCapacityError(error),
		);
	}
	let core: TerminalCore;
	try {
		// Releases the lease itself on throw.
		core = await mgr.createTerminalCoreForLease(lease, baseCols, baseRows);
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		return refuse(
			channelId,
			state,
			`replacement terminal core could not be built: ${message}`,
			isTerminalCoreCapacityError(error),
		);
	}
	if (core.getCols() !== baseCols || core.getRows() !== baseRows) {
		lease.release();
		return refuse(channelId, state, "terminal core did not retain the keeper's base geometry", false);
	}
	if (mgr.sessions.get(channelId) !== rec || mgr.terminalStreams.get(channelId) !== state) {
		lease.release();
		return refuse(channelId, state, SUPERSEDED, true);
	}

	// NO `await` from here to the swap: a JS callback cannot interleave, which
	// is what makes the ring tail splice below exact. session-resume.ts states
	// the same invariant for adoption.
	const liveHeadSeq = rec.head_seq;
	// A view into the old ring, valid only until its next append — consumed
	// synchronously by the splice below, and the old ring is never appended to
	// again after the swap.
	const retained = readRing(rec.scrollback);
	let replay: HistoryReplay;
	try {
		replay = replayKeeperHistory(core, history, legacy, retained, liveHeadSeq);
	} catch (error) {
		lease.release();
		return refuse(
			channelId,
			state,
			error instanceof Error ? error.message : String(error),
			false,
		);
	}
	if (core.getCols() !== terminalState.cols || core.getRows() !== terminalState.rows) {
		lease.release();
		return refuse(
			channelId,
			state,
			"keeper history did not converge to the keeper's reported geometry",
			false,
		);
	}
	// rec.alt_mode is STREAM truth: the retain lane keeps scanning transitions
	// while the core is frozen, so it is authoritative and needs no rescan.
	if (rec.alt_mode && !core.usingAltScreen()) core.writeRaw(ALT_ENTER_SEQS[0]!);
	// Replayed probe answers belong to historical output and must never reach
	// live stdin.
	drainCoreReplies(core);

	const previousLease = rec.terminalCoreLease;
	rec.wtermCore = core;
	rec.terminalCoreLease = lease;
	lease.activate();
	previousLease?.release();
	// A replacement lease holds the capacity owner's single serialized slot
	// until it is completed; leaving it open would refuse every later
	// replacement on this worker, re-proof and keeper-loss respawn alike.
	mgr.terminalCoreCapacity.completeReplacement(lease);
	rec.scrollback = replay.ring;
	rec.head_seq = replay.headSeq;
	// The unhandled-sequence high-water mark is per CORE instance and the fresh
	// core's ring restarts at zero, so a retained mark would mute it for good.
	rec.unhandled = undefined;
	// A new grid identity, not a revision: this history is RE-DERIVED, so a
	// browser must renumber rather than merge a same-epoch full into rows it
	// still holds. sbOrigin stays 0 and sb_origin_pin stays null — the epoch
	// change is what licenses the renumbering, and a floor hit then reports
	// `evicted`, which is the truth after this repair.
	rec.cell_emit = initCellEmitState(newTraceId(), state.streamId);
	mgr.lastAppliedSize.set(channelId, { cols: core.getCols(), rows: core.getRows() });
	state.coreValid = true;

	log.info("session-manager", "terminal_core_reproved", {
		channelId,
		streamId: state.streamId,
		cols: core.getCols(),
		rows: core.getRows(),
		replayedBytes: replay.replayedBytes,
		tailBytes: replay.tailBytes,
		headSeq: replay.headSeq,
		historyEvicted: replay.historyEvicted,
		ordered,
	});
	signal("terminal.core_reproved", {
		sid: String(rec.sessionId),
		channel_id: channelId,
		stream_id: state.streamId,
		replayed_bytes: replay.replayedBytes,
		tail_bytes: replay.tailBytes,
		history_evicted: replay.historyEvicted,
		cooldownKey: String(channelId),
	});
	return { ok: true };
}

/** Every refusal is one warn line plus the verdict the caller maps onto a
 *  stream failure, so no return path can report one without the other. */
function refuse(
	channelId: number,
	state: TerminalStreamState,
	reason: string,
	retryable: boolean,
): TerminalCoreReproofResult {
	log.warn("session-manager", "terminal_core_reproof_failed", {
		channelId,
		streamId: state.streamId,
		reason,
		retryable,
	});
	return { ok: false, reason, retryable };
}

/** Rebuild the retained window and the replacement core's grid from the
 *  keeper's history, then splice on whatever this worker retained past the
 *  keeper's reported head. Synchronous by contract: the caller holds a byte
 *  cursor and a ring view that a live keeper callback would invalidate. */
function replayKeeperHistory(
	core: TerminalCore,
	history: KeeperHistoryRecords | null,
	legacy: LegacyKeeperHistory | null,
	retained: Uint8Array,
	liveHeadSeq: number,
): HistoryReplay {
	const ring = createSbRing();
	let replayedBytes = 0;
	let historyEvicted = false;
	let historyHeadSeq = 0;
	if (history) {
		historyHeadSeq = history.headSeq;
		const retainedTotal = history.records.reduce(
			(total, record) => record.kind === "output" ? total + record.bytes.byteLength : total,
			0,
		);
		historyEvicted = historyHeadSeq > retainedTotal;
		// True only for the cold core's FIRST write, and only under eviction: the
		// keeper's window can open mid-sequence and this parser has no context for
		// that remnant, which would otherwise print as literal text and stick.
		// session-resume.ts carries the full argument.
		let dropOrphanPrefix = historyEvicted;
		for (const record of history.records) {
			if (record.kind === "output") {
				appendToRing(ring, record.bytes);
				core.writeRaw(dropOrphanPrefix
					? record.bytes.subarray(skipOrphanSequencePrefix(record.bytes))
					: record.bytes);
				dropOrphanPrefix = false;
				replayedBytes += record.bytes.byteLength;
				continue;
			}
			if (!isTerminalGeometry({ cols: record.cols, rows: record.rows })) {
				throw new Error("keeper history contains invalid resize geometry");
			}
			// Applied regardless of offset: the PTY already has it, and record
			// order is what preserves correctness.
			core.resize(record.cols, record.rows);
			if (core.getCols() !== record.cols || core.getRows() !== record.rows) {
				throw new Error("terminal core did not retain a keeper history resize");
			}
		}
	} else if (legacy) {
		historyHeadSeq = legacy.headSeq;
		historyEvicted = legacy.headSeq > legacy.bytes.byteLength;
		if (legacy.bytes.byteLength > 0) {
			appendToRing(ring, legacy.bytes);
			core.writeRaw(historyEvicted
				? legacy.bytes.subarray(skipOrphanSequencePrefix(legacy.bytes))
				: legacy.bytes);
			replayedBytes = legacy.bytes.byteLength;
		}
	}
	// The keeper's window closes at its own head. Bytes this worker retained
	// past it exist ONLY in the old ring — the legacy read does not withhold
	// live output, and output keeps arriving while the replacement core is
	// constructed — so splice them on instead of leaving a hole at the head.
	let tailBytes = 0;
	if (liveHeadSeq > historyHeadSeq) {
		tailBytes = liveHeadSeq - historyHeadSeq;
		if (tailBytes > retained.byteLength) {
			throw new Error("captured tail was evicted before the core could be re-proved");
		}
		const tail = retained.subarray(retained.byteLength - tailBytes);
		appendToRing(ring, tail);
		core.writeRaw(tail);
	}
	return {
		ring,
		// The ordered read withholds live output for the request's duration and
		// then DROPS it as already-included history, so the keeper's head can be
		// ahead of ours with no remainder ever arriving on the live lane. The
		// rebuilt window therefore ends at whichever head reaches further.
		headSeq: Math.max(historyHeadSeq, liveHeadSeq),
		replayedBytes,
		tailBytes,
		historyEvicted,
	};
}
