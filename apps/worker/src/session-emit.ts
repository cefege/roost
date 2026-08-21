// Upstream PTY-byte and cell-grid emission. Called with a SessionManager `this`
// (see wrappers in session-manager.ts).

import type { SessionManager } from "./session-manager.ts";
import type { ChannelId } from "@roost/shared/wire";
import { diag, isDiagEnabled, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { asChannelId, DIR_FROM_PTY } from "@roost/shared/wire";
import type { TerminalCore } from "@wterm/core";
import {
	CELL_GRID_PART_MAX_BYTES,
	encodedCellGridFrameSize,
	nextCellFrame,
	scrollbackOrigin,
	type CellEmitState,
} from "@roost/shared/cell";
import { cellFrameToProto } from "@roost/shared/cell/cell-proto";
import type { MuxChannelCallbacks } from "./keeper/multiplexed-client.ts";
import type { TransportSendResult } from "./transport/coord-link-types.ts";
import {
	RECENTLY_CLOSED_TTL_MS,
	KEEPER_DEGRADED_WINDOW_MS,
	KEEPER_DEGRADED_THRESHOLD,
	CELL_EMIT_COALESCE_MS,
	RAW_METADATA_CHANNEL_CAP_BYTES,
	RAW_METADATA_AGGREGATE_CAP_BYTES,
	SYNC_OUTPUT_MAX_MS,
	SYNC_OUTPUT_MAX_PENDING_ROWS,
} from "./session-constants.ts";
import { monoNowMs } from "./util/mono.ts";
import {
	CELL_GATE_BUDGET_MS,
	captureResizeOutput,
	noteGateOverBudget,
} from "./session-resize-capture.ts";
import { noteUnhandledSequences } from "./session-unhandled-seq.ts";
import {
	drainSnapshotCursor,
	installSnapshotCursor,
	renewalHistoryRows,
	validateRenewalHistorySnapshot,
} from "./session-snapshot-cursor.ts";

// Leading-edge sentinel shared by the cell and raw-metadata governors.
const LEADING_SENTINEL = -1 as unknown as NodeJS.Timeout;

// phase-ssb7: emitScrollbackMark + DIR_SCROLLBACK_MARK deleted.
// Splice ordering is now per-byte end_seq on each FROM_PTY frame
// (attachOutputClient.onOutput below). See docs/FAILURE-INDEX.md "scrollback
// seam torn" row.

/** Ingest one PTY chunk synchronously. While a sequenced resize boundary is
 * unresolved (or a resize trap invalidated the core), bytes still enter the
 * bounded recovery record and metadata lanes but never parse at stale geometry. */
export function emitUpstreamChunk(this: SessionManager, channelId: number, chunk: Buffer): void {
	const stream = this.terminalStreams.get(channelId);
	const capture = stream?.resizeCapture ?? null;
	const rec = this.sessions.get(channelId);
	if (rec && rec.lastPtyOutMs === 0) rec.lastPtyOutMs = Date.now();
	diag("cell.recv", { sid: String(rec?.sessionId ?? ""), channel_id: channelId, len: chunk.length });
	if (!this.sendBinaryUpstream) {
		log.warn("session-manager", "emit_no_upstream", {
			channelId,
			len: chunk.length,
		});
		return;
	}
	const endSeq = capture
		? captureResizeOutput(this, channelId, capture, chunk)
		: stream && !stream.coreValid
			? this.appendCapturedScrollback(channelId, chunk)
			: this.appendScrollback(channelId, chunk);
	if (endSeq < 0) {
		const closedAt = this.recentlyClosed.get(channelId);
		if (
			closedAt !== undefined &&
			Date.now() - closedAt < RECENTLY_CLOSED_TTL_MS
		) {
			diag("session.tail_drop", { channel_id: channelId, len: chunk.length });
			return;
		}
		log.warn("session-manager", "emit_no_session", {
			channelId,
			len: chunk.length,
		});
		this._noSessionBurst.push(Date.now());
		const cutoff = Date.now() - KEEPER_DEGRADED_WINDOW_MS;
		while (this._noSessionBurst.length && this._noSessionBurst[0]! < cutoff)
			this._noSessionBurst.shift();
		if (this._noSessionBurst.length >= KEEPER_DEGRADED_THRESHOLD) {
			signal("keeper.degraded", {
				no_session_count: this._noSessionBurst.length,
				window_ms: KEEPER_DEGRADED_WINDOW_MS,
				cooldownKey: "keeper",
			});
			this.onKeeperDegraded?.();
		}
		return;
	}
	this.onTerminalChanged?.(channelId);
	const promoteInputEcho = this.inputSensitiveChannels.delete(channelId);
	if (stream?.enabled) {
		const baselineBoundaryReady = !stream.baselineReady
			&& !stream.snapshotCursor
			&& this.pendingSyncCellSnapshots.has(channelId)
			&& !(rec?.wtermCore.synchronizedOutput?.() ?? false);
		if (baselineBoundaryReady) {
			this.installTerminalBaseline(asChannelId(channelId));
		} else if (!stream.baselineReady || stream.snapshotCursor || capture || !stream.coreValid) {
			stream.baselineDirty = true;
			this.cellDirty.add(channelId);
		} else {
			this._scheduleCellEmit(channelId, promoteInputEcho);
		}
	}
	// Schedule cells first. Its leading microtask/timer is therefore registered
	// ahead of the lower-priority raw metadata lane.
	this._enqueueRawMetadata(channelId, endSeq, chunk);
}

/** Stage coordinator-only raw bytes with strict per-channel and aggregate
 * bounds. The copy is required: Bun may reuse the PTY/ConPTY callback buffer
 * after this synchronous callback returns. */
export function _enqueueRawMetadata(
	this: SessionManager,
	channelId: number,
	endSeq: number,
	chunk: Buffer,
): void {
	let queue = this.rawMetadataQueues.get(channelId);
	if (!queue) {
		queue = { frames: [], bytes: 0 };
		this.rawMetadataQueues.set(channelId, queue);
	}
	if (
		chunk.byteLength > RAW_METADATA_CHANNEL_CAP_BYTES ||
		queue.bytes + chunk.byteLength > RAW_METADATA_CHANNEL_CAP_BYTES ||
		this.rawMetadataQueuedBytes + chunk.byteLength > RAW_METADATA_AGGREGATE_CAP_BYTES
	) {
		diag("transport.frame_dropped", {
			reason: "raw_metadata_stage_overflow",
			kind: "raw",
			channel_id: channelId,
			channel_bytes: queue.bytes,
			aggregate_bytes: this.rawMetadataQueuedBytes,
			frame_bytes: chunk.byteLength,
		});
		signal("transport.raw_metadata_drop", {
			channel_id: channelId,
			reason: "stage_overflow",
			cooldownKey: String(channelId),
		});
		return;
	}
	const stableBytes = Uint8Array.from(chunk);
	queue.frames.push({ endSeq, bytes: stableBytes });
	queue.bytes += stableBytes.byteLength;
	this.rawMetadataQueuedBytes += stableBytes.byteLength;
	if (this.rawMetadataTimers.has(channelId)) return;
	this.rawMetadataTimers.set(channelId, LEADING_SENTINEL);
	queueMicrotask(() => {
		this.rawMetadataTimers.delete(channelId);
		if (!this.sessions.has(channelId)) {
			this._disposeOutputState(channelId);
			return;
		}
		_flushRawMetadata.call(this, channelId);
		_armRawMetadata.call(this, channelId);
	});
}

function _flushRawMetadata(this: SessionManager, channelId: number): void {
	const queue = this.rawMetadataQueues.get(channelId);
	const send = this.sendBinaryUpstream;
	if (!queue || !send) return;
	while (queue.frames.length > 0) {
		const frame = queue.frames[0]!;
		let result: TransportSendResult;
		try {
			result = send(channelId, DIR_FROM_PTY, frame.endSeq, frame.bytes) ?? "sent";
		} catch (error) {
			log.warn("session-manager", "raw_sink_throw", {
				channelId,
				error: error instanceof Error ? error.message : String(error),
			});
			result = "dropped";
		}
		if (result === "dropped") {
			const droppedFrames = queue.frames.length;
			const droppedBytes = queue.bytes;
			queue.frames.length = 0;
			queue.bytes = 0;
			this.rawMetadataQueuedBytes -= droppedBytes;
			diag("transport.frame_dropped", {
				reason: "coordlink_raw_drop",
				kind: "raw",
				channel_id: channelId,
				frames: droppedFrames,
				bytes: droppedBytes,
			});
			signal("transport.raw_metadata_drop", {
				channel_id: channelId,
				reason: "coordlink_outbox",
				cooldownKey: String(channelId),
			});
			return;
		}
		queue.frames.shift();
		queue.bytes -= frame.bytes.byteLength;
		this.rawMetadataQueuedBytes -= frame.bytes.byteLength;
		log.debug("session-manager", "emit_upstream", {
			channelId,
			len: frame.bytes.byteLength,
			endSeq: frame.endSeq,
			result,
		});
	}
}

function _armRawMetadata(this: SessionManager, channelId: number): void {
	const timer = setTimeout(() => {
		this.rawMetadataTimers.delete(channelId);
		if (!this.sessions.has(channelId)) {
			this._disposeOutputState(channelId);
			return;
		}
		const queue = this.rawMetadataQueues.get(channelId);
		if (!queue || queue.frames.length === 0) return;
		_flushRawMetadata.call(this, channelId);
		_armRawMetadata.call(this, channelId);
	}, CELL_EMIT_COALESCE_MS);
	this.rawMetadataTimers.set(channelId, timer);
}

export function resumeTerminalSnapshots(this: SessionManager): void {
	for (const [channelId, state] of this.terminalStreams) {
		if (!this.sessions.has(channelId) || !state.enabled || !state.coreValid) continue;
		if (state.snapshotCursor) drainSnapshotCursor(this, channelId, state);
		else if (this.pendingCellRepairs.delete(channelId)) this.installTerminalBaseline(asChannelId(channelId));
	}
}

/** Which gate is withholding cell frames for a channel, since when (monotonic),
 *  and how many frames it has suppressed. A stalled emitter is then attributable
 *  from the diagnostic snapshot alone instead of by correlating logs. */
export interface CellGateSuppression {
	gate: "resize_capture" | "baseline" | "sync_output";
	sinceMonoMs: number;
	frames: number;
	/** The gate outlived its own ceiling: a resize/repair gate past the keeper
	 *  command budget (corruption), or a synchronized-output hold past its cap
	 *  (the withheld frame shipped and the stuck generation is bypassed). */
	overBudget: boolean;
	/** The ceiling `overBudget` is measured against. Per gate, because the
	 *  synchronized-output hold answers to its own cap, not the keeper's. */
	budgetMs: number;
}

function noteCellGateSuppression(
	mgr: SessionManager,
	channelId: number,
	gate: CellGateSuppression["gate"],
): void {
	const now = monoNowMs();
	const budgetMs = gate === "sync_output" ? SYNC_OUTPUT_MAX_MS : CELL_GATE_BUDGET_MS;
	let state = mgr.cellGateSuppression.get(channelId);
	if (!state || state.gate !== gate) {
		state = { gate, sinceMonoMs: now, frames: 0, overBudget: false, budgetMs };
		mgr.cellGateSuppression.set(channelId, state);
	}
	state.frames++;
	const ageMs = now - state.sinceMonoMs;
	// Past the keeper's own per-command budget the gate is no longer explainable
	// by one in-flight command; that is corruption, not latency. A
	// synchronized-output hold trips on its armed timer instead — firing IS the
	// expiry, so it never depends on a chunk arriving to re-read the clock.
	if (gate === "sync_output" || state.overBudget || ageMs <= state.budgetMs) return;
	state.overBudget = true;
	noteGateOverBudget(mgr, channelId, ageMs);
}

/** One DEC 2026 synchronized-output frame whose intermediate cell sends the
 *  emitter is withholding. Bounded by a wall ceiling (the armed timer) and a
 *  pending-row ceiling; whichever trips first emits the withheld frame and
 *  stops this generation suppressing anything further. */
export interface SyncOutputHold {
	/** The core's synchronized-output generation this hold belongs to. */
	generation: number;
	/** Monotonic scrollback total when the hold opened; the work ceiling is
	 *  measured against it. */
	sbTotalAtOpen: number;
	/** Fires exactly at the wall ceiling. A producer that goes SILENT inside an
	 *  unterminated frame is the case only this timer can recover. */
	timer: NodeJS.Timeout | undefined;
	tripped: boolean;
}

/** What the streaming path must do with this chunk's frame. */
type SyncOutputAction =
	/** No synchronized frame is withholding anything: use the rate governor. */
	| "pass"
	/** Inside a synchronized frame with both ceilings intact: withhold. */
	| "hold"
	/** A boundary the browser is owed the withheld frame at: the application
	 *  closed a frame we withheld, or a ceiling just tripped. */
	| "flush";

function syncOutputAction(mgr: SessionManager, channelId: number): SyncOutputAction {
	const rec = mgr.sessions.get(channelId);
	const core = rec?.wtermCore;
	if (!rec || !core) return "pass";
	const hold = mgr.syncOutputHolds.get(channelId);
	if (!(core.synchronizedOutput?.() ?? false)) {
		if (hold === undefined) return "pass";
		// The application closed its frame. A hold that actually withheld output
		// owes the browser one authoritative frame at the boundary the
		// application itself declared. A tripped hold normally already flushed,
		// except when that ceiling emission was withheld because it would have
		// resolved a pending full snapshot from the intermediate grid.
		const owed = !hold.tripped || mgr.pendingSyncCellSnapshots.has(channelId);
		releaseSyncOutputHold(mgr, channelId);
		return owed ? "flush" : "pass";
	}
	const generation = core.synchronizedOutputGeneration?.() ?? 0;
	if (hold !== undefined && hold.generation === generation) {
		if (hold.tripped) return "pass";
		if (syncPendingRows(core, rec.cell_emit, hold) < SYNC_OUTPUT_MAX_PENDING_ROWS) return "hold";
		tripSyncOutputHold(mgr, channelId, hold, "pending_rows");
		return "flush";
	}
	if (hold !== undefined && !hold.tripped) {
		// Closed and immediately reopened inside one chunk, with nothing emitted
		// in between: the browser has been continuously dark, so the new
		// generation INHERITS the running ceilings. Restarting them here is what
		// would let a `2026l 2026h` loop suppress forever one reset at a time.
		hold.generation = generation;
		return "hold";
	}
	if (hold !== undefined) releaseSyncOutputHold(mgr, channelId);
	mgr.syncOutputHolds.set(
		channelId,
		openSyncOutputHold(mgr, channelId, generation, monoScrollbackTotal(core, rec.cell_emit)),
	);
	return "hold";
}

/** Roost's monotonic scrollback total: the authoritative eviction origin plus
 *  everything the ring still holds. The origin is what keeps this monotonic at
 *  saturation, where getScrollbackCount() alone pins and stops moving. */
function monoScrollbackTotal(core: TerminalCore, emit: CellEmitState): number {
	return scrollbackOrigin(core, emit) + core.getScrollbackCount();
}

/** Watch the core's fixed OSC 8 link table for the one transition that is
 *  otherwise invisible. At saturation the terminal keeps painting perfectly and
 *  every NEW distinct hyperlink silently degrades to plain text — no error, no
 *  missing output, just links that stop appearing. Edge-triggered off a
 *  per-channel flag so one flip is one signal; a core rebuild empties the table
 *  and the next frame clears the flag, re-arming the next real flip. */
function noteHyperlinkSaturation(mgr: SessionManager, channelId: number, core: TerminalCore, sid: string): void {
	const links = core.getResourceState?.().hyperlinks;
	if (links === undefined) return;
	const had = mgr.hyperlinkSaturated.has(channelId);
	if (!links.saturated) {
		if (had) mgr.hyperlinkSaturated.delete(channelId);
		return;
	}
	if (had) return;
	mgr.hyperlinkSaturated.add(channelId);
	signal("terminal.hyperlink_saturated", {
		sid,
		channel_id: channelId,
		capacity: links.capacity,
		used: links.used,
		rejected: links.rejected,
		cooldownKey: sid,
	});
}

/** Rows the browser is missing inside this frame: history appended since the
 *  hold opened, plus the viewport rows currently dirty. Costs one WASM read per
 *  viewport row, and only on the suppressed path. */
function syncPendingRows(core: TerminalCore, emit: CellEmitState, hold: SyncOutputHold): number {
	const rows = core.getRows();
	let dirty = 0;
	for (let row = 0; row < rows; row++) if (core.isDirtyRow(row)) dirty++;
	return Math.max(0, monoScrollbackTotal(core, emit) - hold.sbTotalAtOpen) + dirty;
}

function openSyncOutputHold(
	mgr: SessionManager,
	channelId: number,
	generation: number,
	sbTotalAtOpen: number,
): SyncOutputHold {
	const hold: SyncOutputHold = { generation, sbTotalAtOpen, timer: undefined, tripped: false };
	// Armed for exactly the ceiling, so FIRING is the expiry: the decision never
	// re-reads a clock, and a host clock step can neither forge nor hide it.
	hold.timer = setTimeout(() => {
		hold.timer = undefined;
		if (mgr.syncOutputHolds.get(channelId) !== hold) return;
		// The trip itself is what unblocks the emit below (it clears
		// `tripped === false`), so this ships UNFORCED and lets nextCellFrame
		// choose delta-vs-full. Nothing cleared the withheld rows' dirty bits and
		// nothing advanced rec.cell_emit, so a delta still carries every row and
		// every scrolled-off line the suppression accumulated.
		tripSyncOutputHold(mgr, channelId, hold, "elapsed_ms");
		mgr.emitCellFrame(asChannelId(channelId), false);
	}, SYNC_OUTPUT_MAX_MS);
	return hold;
}

/** Stop withholding for this generation and leave the trip legible in the
 *  diagnostic snapshot: gate, age and suppressed-frame count stay put until the
 *  application finally closes its frame, because a terminal stuck inside an
 *  unterminated synchronized frame is exactly what an operator needs to see. */
function tripSyncOutputHold(
	mgr: SessionManager,
	channelId: number,
	hold: SyncOutputHold,
	cap: "elapsed_ms" | "pending_rows",
): void {
	hold.tripped = true;
	clearTimeout(hold.timer);
	hold.timer = undefined;
	const state = mgr.cellGateSuppression.get(channelId);
	if (state?.gate === "sync_output") state.overBudget = true;
	signal("terminal.sync_output_cap", {
		sid: String(mgr.sessions.get(channelId)?.sessionId ?? ""),
		channel_id: channelId,
		cap,
		generation: hold.generation,
		age_ms: state ? Math.round(monoNowMs() - state.sinceMonoMs) : 0,
		suppressed_frames: state?.frames ?? 0,
		cap_ms: SYNC_OUTPUT_MAX_MS,
		cap_rows: SYNC_OUTPUT_MAX_PENDING_ROWS,
		cooldownKey: String(channelId),
	});
}

/** Stop withholding and forget the generation entirely. Exported because the
 *  resize transaction has to retire a hold from OUTSIDE the streaming path: a
 *  hold's `generation` and `sbTotalAtOpen` are expressed in ONE core instance's
 *  terms, so both points where that instance stops being what the emitter reads
 *  — the capture that freezes it, and the swap that replaces it — must retire
 *  it. Left armed across either, its wall timer fires an emit into the resize
 *  gate, which emitCellFrame checks BEFORE the force bypass and therefore
 *  discards: the 1 s recovery ceiling is spent invisibly inside the transaction,
 *  and the next chunk then either inherits a dead core's generation or opens a
 *  fresh 1 s ceiling stacked on top of the resize's own budget. */
export function releaseSyncOutputHold(mgr: SessionManager, channelId: number): void {
	const hold = mgr.syncOutputHolds.get(channelId);
	if (hold === undefined) return;
	clearTimeout(hold.timer);
	mgr.syncOutputHolds.delete(channelId);
	if (mgr.cellGateSuppression.get(channelId)?.gate === "sync_output") {
		mgr.cellGateSuppression.delete(channelId);
	}
}

export function _disposeOutputState(this: SessionManager, channelId: number): void {
	const rawTimer = this.rawMetadataTimers.get(channelId);
	if (rawTimer !== undefined && rawTimer !== LEADING_SENTINEL) clearTimeout(rawTimer);
	this.rawMetadataTimers.delete(channelId);
	const queue = this.rawMetadataQueues.get(channelId);
	if (queue) this.rawMetadataQueuedBytes -= queue.bytes;
	this.rawMetadataQueues.delete(channelId);
	this.inputSensitiveChannels.delete(channelId);
	this.pendingCellRepairs.delete(channelId);
	this.pendingSyncCellSnapshots.delete(channelId);
	this.cellDirty.delete(channelId);
	this.cellGateSuppression.delete(channelId);
	releaseSyncOutputHold(this, channelId);
}


/** Cell production is gated by the coordinator-owned stream state, never by
 * per-viewer claims in the worker. */
export function _hasEnabledStream(this: SessionManager, channelId: number): boolean {
	const stream = this.terminalStreams.get(channelId);
	return stream?.enabled === true && stream.coreValid;
}

/** Rate governor: leading-edge cell emit plus trailing coalesce. A single
 * input-sensitive return chunk may replace an armed trailing timer with the
 * existing leading microtask; the governor re-arms from that promoted echo. */
export function _scheduleCellEmit(
	this: SessionManager,
	channelId: number,
	promoteInputEcho = false,
): void {
	const stream = this.terminalStreams.get(channelId);
	if (!stream?.enabled || !stream.coreValid) return;
	if (!stream.baselineReady || stream.snapshotCursor) {
		stream.baselineDirty = true;
		this.cellDirty.add(channelId);
		noteCellGateSuppression(this, channelId, "baseline");
		return;
	}
	if (this.cellEmissionGates.has(channelId)) {
		this.cellDirty.add(channelId);
		noteCellGateSuppression(this, channelId, "resize_capture");
		return;
	}
	if (this.pendingCellRepairs.delete(channelId)) {
		this.installTerminalBaseline(asChannelId(channelId));
		return;
	}
	switch (syncOutputAction(this, channelId)) {
		case "hold": {
			this.cellDirty.add(channelId);
			noteCellGateSuppression(this, channelId, "sync_output");
			// A trailing coalesce timer armed BEFORE the frame opened would
			// otherwise keep firing straight through it, re-arming each time —
			// suppression that leaks one frame per 16 ms is not suppression. The
			// hold's own ceiling is the only timer allowed to produce a frame now.
			const armed = this.cellEmitTimers.get(channelId);
			if (armed !== undefined && armed !== LEADING_SENTINEL) {
				clearTimeout(armed);
				this.cellEmitTimers.delete(channelId);
			}
			return;
		}
		case "flush":
			// The application closed the frame it opened, or a ceiling refused to
			// stay dark any longer. Either way the browser gets the withheld state
			// at that boundary, now, outside the coalesce governor — the next chunk
			// starts a fresh leading edge. UNFORCED: suppression never touched
			// rec.cell_emit, so the emitter's own reframe test still describes
			// exactly the frame the browser is holding, and a delta both costs a
			// fraction of a full-viewport read and carries the history that
			// scrolled off mid-burst INLINE — a full frame carries no history at
			// all, so those lines would come back as a separate backfill trip.
			this.emitCellFrame(channelId, false);
			return;
		case "pass":
			break;
	}
	const pending = this.cellEmitTimers.get(channelId);
	if (pending !== undefined) {
		this.cellDirty.add(channelId);
		if (!promoteInputEcho || pending === LEADING_SENTINEL) return;
		clearTimeout(pending);
		this.cellEmitTimers.delete(channelId);
	}

	this.cellEmitTimers.set(channelId, LEADING_SENTINEL);
	queueMicrotask(() => {
		this.cellEmitTimers.delete(channelId);
		if (!this.sessions.has(channelId)) return;
		this.emitCellFrame(channelId, false);
		if (this.pendingCellRepairs.has(channelId)) return;
		const arm = (): void => {
			const timer = setTimeout(() => {
				this.cellEmitTimers.delete(channelId);
				if (
					!this.sessions.has(channelId) ||
					this.pendingCellRepairs.has(channelId) ||
					!this.cellDirty.has(channelId)
				) return;
				this.emitCellFrame(channelId, false);
				if (!this.pendingCellRepairs.has(channelId)) arm();
			}, CELL_EMIT_COALESCE_MS);
			this.cellEmitTimers.set(channelId, timer);
		};
		arm();
	});
}

export function installTerminalBaseline(this: SessionManager, channelId: number): void {
	this.emitCellFrame(channelId, true);
}

/** Emit a delta only after a complete baseline. A full is installed as a
 * cancellable immutable cursor; oversized deltas promote to that same path. */
export function emitCellFrame(this: SessionManager, channelId: number, force: boolean): void {
	const state = this.terminalStreams.get(channelId);
	const rec = this.sessions.get(channelId);
	if (!state?.enabled || !state.coreValid || !rec) return;
	if (this.cellEmissionGates.has(channelId)) {
		state.baselineDirty = true;
		this.cellDirty.add(channelId);
		noteCellGateSuppression(this, channelId, "resize_capture");
		return;
	}
	if (state.snapshotCursor) {
		if (!force) state.baselineDirty = true;
		return;
	}
	if (!force && !state.baselineReady) {
		state.baselineDirty = true;
		this.cellDirty.add(channelId);
		noteCellGateSuppression(this, channelId, "baseline");
		return;
	}
	const core = rec.wtermCore;
	const fullOwed = force || !rec.cell_emit.sentFull || this.pendingSyncCellSnapshots.has(channelId);
	const syncAction = syncOutputAction(this, channelId);
	const deferFull = (core.synchronizedOutput?.() ?? false) && fullOwed;
	if (deferFull) this.pendingSyncCellSnapshots.add(channelId);
	if (syncAction === "hold" || deferFull) {
		state.baselineDirty = true;
		this.cellDirty.add(channelId);
		noteCellGateSuppression(this, channelId, "sync_output");
		return;
	}
	const pending = this.cellEmitTimers.get(channelId);
	if (pending !== undefined) {
		if (pending !== LEADING_SENTINEL) clearTimeout(pending);
		this.cellEmitTimers.delete(channelId);
	}
	let tailRows = 0;
	if (fullOwed && rec.cell_emit.sentFull && rec.cell_emit.seq === 0) {
		tailRows = renewalHistoryRows(core, rec.cell_emit);
	}
	let next = nextCellFrame(core, rec.cell_emit, fullOwed, tailRows);
	let pb = cellFrameToProto(next.frame, String(rec.sessionId));
	pb.ptyOutMs = BigInt(rec.lastPtyOutMs || Date.now());
	pb.workerEmitMs = BigInt(Date.now());
	if (!next.frame.full && encodedCellGridFrameSize(pb) > CELL_GRID_PART_MAX_BYTES) {
		next = nextCellFrame(core, rec.cell_emit, true, tailRows);
		pb = cellFrameToProto(next.frame, String(rec.sessionId));
		pb.ptyOutMs = BigInt(rec.lastPtyOutMs || Date.now());
		pb.workerEmitMs = BigInt(Date.now());
	}
	if (tailRows > 0 && !validateRenewalHistorySnapshot(pb)) {
		rec.cell_emit.gridEpochRevision++;
		tailRows = 0;
		next = nextCellFrame(core, rec.cell_emit, true, 0);
		pb = cellFrameToProto(next.frame, String(rec.sessionId));
		pb.ptyOutMs = BigInt(rec.lastPtyOutMs || Date.now());
		pb.workerEmitMs = BigInt(Date.now());
	}
	noteHyperlinkSaturation(this, channelId, core, String(rec.sessionId));
	noteUnhandledSequences(rec, core);
	if (next.frame.full) {
		rec.cell_emit = next.state;
		core.clearDirty();
		this.cellDirty.delete(channelId);
		state.baselineDirty = false;
		if (!installSnapshotCursor(this, channelId, state, pb)) return;
	} else {
		const result = this.sendCellGridUpstream?.(channelId, pb) ?? "dropped";
		if (result === "dropped") {
			this.pendingCellRepairs.add(channelId);
			state.baselineReady = false;
			this.installTerminalBaseline(asChannelId(channelId));
			return;
		}
		rec.cell_emit = next.state;
		core.clearDirty();
		this.cellDirty.delete(channelId);
	}
	rec.lastPtyOutMs = 0;
	if (isDiagEnabled()) {
		diag("cell.emit", {
			sid: String(rec.sessionId),
			stream_id: state.streamId,
			seq: next.frame.seq,
			base_seq: next.frame.baseSeq,
			full: next.frame.full,
			vp_rows: next.frame.viewportRows.length,
			result: state.snapshotCursor ? "cursor" : "sent",
		});
	}
}

/** Register per-channel output handlers on the multiplexed pool. */
export function muxCallbacks(this: SessionManager, channelId: number): MuxChannelCallbacks {
	return {
		onOutput: (chunk: Buffer) => this.emitUpstreamChunk(channelId, chunk),
		onExit: (exitCode) => {
			if (exitCode === null) {
				const sid = this.sessions.get(channelId)?.sessionId;
				diag("session.exit_null", { sid, channel_id: channelId });
				return;
			}
			this.closedByKeeper(channelId as ChannelId, exitCode);
		},
		onError: (err: Error) =>
			log.warn("session-manager", "mux_channel_err", {
				channelId,
				err: err.message,
			}),
	};
}
