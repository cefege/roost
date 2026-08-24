// Live resize capture for the cell emitter: while a sequenced resize boundary
// is unresolved, incoming PTY bytes are captured instead of parsed at stale
// geometry, then replayed once the new size is proven — protecting the core
// from mid-repaint geometry flips. Owns the capture gate budget that flags
// gates overstaying their ceiling.
import { diag, signal } from "@roost/shared/diag";
import { newTraceId } from "@roost/shared/trace";
import type { SessionManager } from "./session-manager.ts";
import type { LiveResizeCapture, TerminalStreamState } from "./session-terminal-state.ts";
import type { KeeperHistoryRecords, KeeperResizeResult } from "./keeper/multiplexed-client.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { answerQueries } from "./terminal-query-reply.ts";
import { readRing } from "./session-scrollback-ring.ts";

export const CELL_GATE_BUDGET_MS = 2_500;

export function installLiveResizeCapture(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
	resizeSeq: number,
	fromCols: number,
	fromRows: number,
	toCols: number,
	toRows: number,
): LiveResizeCapture {
	const rec = mgr.sessions.get(channelId);
	if (!rec) throw new Error("session closed before resize capture");
	const capture: LiveResizeCapture = {
		streamId: state.streamId,
		resizeSeq,
		installSeq: rec.head_seq,
		fromCols,
		fromRows,
		toCols,
		toRows,
		queryCarry: rec.query_carry.slice(),
		capturedBytes: 0,
		capturedChunks: 0,
		boundarySeq: -1,
		boundaryApplied: false,
		failedReason: null,
	};
	state.resizeCapture = capture;
	mgr.cellEmissionGates.add(channelId);
	mgr._releaseSyncOutputHold(channelId);
	return capture;
}

/** Retain and scan bytes while the keeper boundary is unresolved, never parsing
 * them through a core whose geometry is not yet proven. */
export function captureResizeOutput(
	mgr: SessionManager,
	channelId: number,
	capture: LiveResizeCapture,
	chunk: Buffer,
): number {
	const endSeq = mgr.appendCapturedScrollback(channelId, chunk);
	if (endSeq >= 0) {
		capture.capturedBytes += chunk.byteLength;
		capture.capturedChunks += 1;
	}
	return endSeq;
}

function capturedBytes(
	mgr: SessionManager,
	channelId: number,
	capture: LiveResizeCapture,
	throughSeq: number,
): Uint8Array | null {
	const rec = mgr.sessions.get(channelId);
	if (!rec || throughSeq < capture.installSeq || throughSeq > rec.head_seq) return null;
	const retained = readRing(rec.scrollback);
	const retainedStart = rec.head_seq - retained.byteLength;
	if (capture.installSeq < retainedStart) return null;
	return retained.subarray(
		capture.installSeq - retainedStart,
		throughSeq - retainedStart,
	);
}

function forwardReplies(channelId: number, replies: string): void {
	if (replies.length === 0) return;
	getMultiplexedPool().input(channelId, new TextEncoder().encode(replies));
}

function resetEmissionEpoch(mgr: SessionManager, channelId: number): void {
	const rec = mgr.sessions.get(channelId);
	if (!rec) return;
	rec.cell_emit = {
		...rec.cell_emit,
		gridEpochBase: newTraceId(),
		gridEpochRevision: 0,
		sentFull: false,
		cols: 0,
		rows: 0,
		alt: false,
	};
	const stream = mgr.terminalStreams.get(channelId);
	if (stream) {
		stream.baselineReady = false;
		stream.baselineDirty = true;
		stream.snapshotCursor = null;
	}
	for (let row = 0; row < rec.wtermCore.getRows(); row += 1) {
		// The patched core marks every viewport row dirty during resize.
		if (!rec.wtermCore.isDirtyRow(row)) {
			throw new Error(`terminal core resize left row ${row} clean`);
		}
	}
}

function failCore(
	mgr: SessionManager,
	channelId: number,
	capture: LiveResizeCapture,
	reason: string,
): void {
	capture.failedReason = reason;
	const stream = mgr.terminalStreams.get(channelId);
	if (stream) {
		stream.coreValid = false;
		stream.baselineReady = false;
		stream.snapshotCursor = null;
	}
	signal("terminal.core_failed", {
		sid: String(mgr.sessions.get(channelId)?.sessionId ?? ""),
		channel_id: channelId,
		stream_id: capture.streamId,
		resize_seq: capture.resizeSeq,
		reason,
		cooldownKey: String(channelId),
	});
}

function finishCapture(mgr: SessionManager, channelId: number, capture: LiveResizeCapture): void {
	const stream = mgr.terminalStreams.get(channelId);
	if (stream?.resizeCapture === capture) stream.resizeCapture = null;
	mgr.cellEmissionGates.delete(channelId);
	mgr.cellGateSuppression.delete(channelId);
}

/** Runs synchronously inside ResizeAck/ResizeReject dispatch, before a later
 * PtyOut from the same socket read. It catches every core trap so bytes can never
 * fall through to parsing at stale geometry. */
export function applyResizeResultAtBoundary(
	mgr: SessionManager,
	channelId: number,
	capture: LiveResizeCapture,
	result: KeeperResizeResult,
): void {
	if (capture.boundaryApplied || capture.failedReason) return;
	const rec = mgr.sessions.get(channelId);
	if (!rec) return;
	capture.boundarySeq = rec.head_seq;
	const beforeBoundary = capturedBytes(mgr, channelId, capture, capture.boundarySeq);
	if (!beforeBoundary) {
		failCore(mgr, channelId, capture, "resize boundary output was evicted before alignment");
		return;
	}
	try {
		const replayState = { query_carry: capture.queryCarry.slice() };
		const replies = answerQueries(replayState, rec.wtermCore, beforeBoundary).bytes;
		if (result.kind === "ack") {
			if (result.seq !== capture.resizeSeq
				|| result.cols !== capture.toCols
				|| result.rows !== capture.toRows) {
				throw new Error("keeper acknowledged conflicting resize geometry");
			}
			rec.wtermCore.resize(capture.toCols, capture.toRows);
			if (rec.wtermCore.getCols() !== capture.toCols
				|| rec.wtermCore.getRows() !== capture.toRows) {
				throw new Error("terminal core did not retain validated resize geometry");
			}
			resetEmissionEpoch(mgr, channelId);
		}
		capture.boundaryApplied = true;
		finishCapture(mgr, channelId, capture);
		forwardReplies(channelId, replies);
	} catch (error) {
		failCore(
			mgr,
			channelId,
			capture,
			error instanceof Error ? error.message : String(error),
		);
	}
}

/** Lost-ACK recovery. Keeper history is an ordered output/resize record stream.
 * Replay only the suffix the existing core has not parsed, resize that same core
 * at the retained target marker, and fail closed if the boundary was evicted. */
export async function recoverAmbiguousResize(
	mgr: SessionManager,
	channelId: number,
	capture: LiveResizeCapture,
): Promise<{ ok: true } | { ok: false; reason: string }> {
	let history: KeeperHistoryRecords;
	try {
		history = await getMultiplexedPool().getHistoryRecords(channelId);
	} catch (error) {
		const reason = `ordered resize history unavailable: ${error instanceof Error ? error.message : String(error)}`;
		failCore(mgr, channelId, capture, reason);
		return { ok: false, reason };
	}
	const rec = mgr.sessions.get(channelId);
	if (!rec) return { ok: false, reason: "session closed during resize recovery" };
	const retainedBytes = history.records.reduce(
		(total, record) => total + (record.kind === "output" ? record.bytes.byteLength : 0),
		0,
	);
	const retainedStart = history.headSeq - retainedBytes;
	if (capture.installSeq < retainedStart) {
		const reason = "ordered resize boundary was evicted";
		failCore(mgr, channelId, capture, reason);
		return { ok: false, reason };
	}
	let outputSeq = retainedStart;
	let applied = false;
	const replayState = { query_carry: capture.queryCarry.slice() };
	let replies = "";
	try {
		for (const record of history.records) {
			if (record.kind === "output") {
				const outputEnd = outputSeq + record.bytes.byteLength;
				if (outputEnd > capture.installSeq) {
					const from = Math.max(0, capture.installSeq - outputSeq);
					replies += answerQueries(replayState, rec.wtermCore, record.bytes.subarray(from)).bytes;
				}
				outputSeq = outputEnd;
				continue;
			}
			if (record.seq !== capture.resizeSeq) {
				if (outputSeq > capture.installSeq) {
					throw new Error(`unexpected resize ${record.seq} inside recovery suffix`);
				}
				continue;
			}
			if (applied) throw new Error("duplicate resize boundary in ordered history");
			rec.wtermCore.resize(record.cols, record.rows);
			if (rec.wtermCore.getCols() !== capture.toCols
				|| rec.wtermCore.getRows() !== capture.toRows
				|| record.cols !== capture.toCols
				|| record.rows !== capture.toRows) {
				throw new Error("ordered history contained conflicting resize geometry");
			}
			resetEmissionEpoch(mgr, channelId);
			applied = true;
		}
		if (!applied) throw new Error("ordered resize boundary was not retained");
		capture.boundarySeq = history.headSeq;
		capture.boundaryApplied = true;
		finishCapture(mgr, channelId, capture);
		forwardReplies(channelId, replies);
		diag("resize.wterm_core", {
			sid: rec.sessionId,
			channel_id: channelId,
			stream_id: capture.streamId,
			resize_seq: capture.resizeSeq,
			cols: capture.toCols,
			rows: capture.toRows,
			mode: "in_place_ordered_recovery",
		});
		return { ok: true };
	} catch (error) {
		const reason = error instanceof Error ? error.message : String(error);
		failCore(mgr, channelId, capture, reason);
		return { ok: false, reason };
	}
}

export function noteGateOverBudget(mgr: SessionManager, channelId: number, ageMs: number): void {
	const stream = mgr.terminalStreams.get(channelId);
	signal("terminal.gate_over_budget", {
		sid: String(mgr.sessions.get(channelId)?.sessionId ?? ""),
		channel_id: channelId,
		stream_id: stream?.streamId ?? "",
		age_ms: Math.round(ageMs),
		budget_ms: CELL_GATE_BUDGET_MS,
		reason: stream?.resizeCapture ? "resize_boundary" : "baseline",
		cooldownKey: String(channelId),
	});
}
