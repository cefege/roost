// Upstream PTY-byte and cell-grid emission for SessionManager.
// Cells retain their cadence; metadata records use the negotiated semantic lane
// or bounded old-coordinator raw compatibility state after each chunk.
// Synchronized-output holds remain owned by session-sync-output.ts.

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
import {
	RECENTLY_CLOSED_TTL_MS,
	KEEPER_DEGRADED_WINDOW_MS,
	KEEPER_DEGRADED_THRESHOLD,
} from "./session-constants.ts";
import { captureResizeOutput } from "./session-resize-capture.ts";
import {
	cancelCellEmission,
	consumeInputEchoPromotion,
	noteCellGateSuppression,
} from "./session-cell-scheduler.ts";
import { noteUnhandledSequences } from "./session-unhandled-seq.ts";
import {
	drainSnapshotCursor,
	installStreamBaseline,
	prepareCellRenewalEpoch,
} from "./session-snapshot-cursor.ts";
import {
	activeCellSinks,
	aggregateStreamDelivery,
	clearStreamDeliveryDirty,
	invalidateStreamBaselines,
	markStreamDeliveryDirty,
	sendCellDeltaToSinks,
} from "./session-cell-sinks.ts";
import { disposeRawMetadataState } from "./session-raw-metadata.ts";
import {
	disposeTerminalMetadataState,
	observeTerminalMetadata,
} from "./session-terminal-metadata.ts";
import { releaseSyncOutputHold, syncOutputAction } from "./session-sync-output.ts";
import {
	noteAcceptedCellEmission,
	noteRejectedCellEmission,
} from "./diag/terminal-capture.ts";


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
	if (
		!this.sendBinaryUpstream
		&& !this.sendTerminalMetadataUpstream
		&& this.cellSinks.size === 0
	) {
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
	const promoteInputEcho = consumeInputEchoPromotion(this, channelId);
	if (stream?.enabled) {
		const delivery = aggregateStreamDelivery(this, stream);
		const baselineBoundaryReady = !delivery.baselineReady
			&& !delivery.snapshotPending
			&& this.pendingSyncCellSnapshots.has(channelId)
			&& !(rec?.wtermCore.synchronizedOutput?.() ?? false);
		if (baselineBoundaryReady) {
			this.installTerminalBaseline(asChannelId(channelId));
		} else if (
			!delivery.baselineReady || delivery.snapshotPending || capture || !stream.coreValid
		) {
			markStreamDeliveryDirty(this, stream);
			this.cellDirty.add(channelId);
		} else {
			this._scheduleCellEmit(channelId, promoteInputEcho);
		}
	}
	// Cell scheduling is registered before semantic metadata publication.
	observeTerminalMetadata(this, channelId, chunk);
	if (!this.terminalMetadataNegotiated) this._enqueueRawMetadata(channelId, endSeq, chunk);
}
export function resumeTerminalSnapshots(this: SessionManager): void {
	const sinks = activeCellSinks(this);
	for (const [channelId, state] of this.terminalStreams) {
		if (!this.sessions.has(channelId) || !state.enabled || !state.coreValid) continue;
		let parked = false;
		for (const sink of sinks) {
			if (!state.deliveries.get(sink.id)?.cursor) continue;
			parked = true;
			drainSnapshotCursor(this, channelId, state, sink.id);
		}
		if (!parked && this.pendingCellRepairs.delete(channelId)) {
			this.installTerminalBaseline(asChannelId(channelId));
		}
	}
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
export function _disposeOutputState(this: SessionManager, channelId: number): void {
	// Compatibility and semantic metadata retain independent per-channel state.
	disposeRawMetadataState(this, channelId);
	disposeTerminalMetadataState(this, channelId);
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


export function installTerminalBaseline(this: SessionManager, channelId: number): void {
	this.emitCellFrame(channelId, true);
}

/** Emit a delta only after every active sink holds a complete baseline. ONE
 * frame is built per tick — a second CellEmitState over one core would steal
 * the dirty rows this frame claimed — and then fanned to each active sink: a
 * full parks as that sink's cancellable cursor, a delta ships directly.
 * Oversized deltas promote to the same full path. */
export function emitCellFrame(this: SessionManager, channelId: number, force: boolean): void {
	const state = this.terminalStreams.get(channelId);
	const rec = this.sessions.get(channelId);
	if (!state?.enabled || !state.coreValid || !rec) return;
	const delivery = aggregateStreamDelivery(this, state);
	// No active sink: a suspended transport must not latch repairs or force
	// baselines. Its resume owes one full, so nothing needs recording here.
	if (delivery.activeSinks === 0) return;
	if (this.cellEmissionGates.has(channelId)) {
		markStreamDeliveryDirty(this, state);
		this.cellDirty.add(channelId);
		noteCellGateSuppression(this, channelId, "resize_capture");
		return;
	}
	if (delivery.snapshotPending) {
		if (!force) markStreamDeliveryDirty(this, state);
		return;
	}
	if (!force && !delivery.baselineReady) {
		markStreamDeliveryDirty(this, state);
		this.cellDirty.add(channelId);
		noteCellGateSuppression(this, channelId, "baseline");
		return;
	}
	const core = rec.wtermCore;
	const fullOwed = force || !rec.cell_emit.sentFull || this.pendingSyncCellSnapshots.has(channelId);
	const syncAction = syncOutputAction(this, channelId);
	const deferFull = syncAction === "hold" && fullOwed;
	if (deferFull) this.pendingSyncCellSnapshots.add(channelId);
	if (syncAction === "hold" || deferFull) {
		markStreamDeliveryDirty(this, state);
		this.cellDirty.add(channelId);
		noteCellGateSuppression(this, channelId, "sync_output");
		return;
	}
	cancelCellEmission(this, channelId);
	if (fullOwed && rec.cell_emit.sentFull && rec.cell_emit.seq === 0) {
		prepareCellRenewalEpoch(core, rec.cell_emit);
	}
	let next = nextCellFrame(core, rec.cell_emit, fullOwed, 0);
	let pb = cellFrameToProto(next.frame, String(rec.sessionId));
	pb.ptyOutMs = BigInt(rec.lastPtyOutMs || Date.now());
	pb.workerEmitMs = BigInt(Date.now());
	if (!next.frame.full && encodedCellGridFrameSize(pb) > CELL_GRID_PART_MAX_BYTES) {
		next = nextCellFrame(core, rec.cell_emit, true, 0);
		pb = cellFrameToProto(next.frame, String(rec.sessionId));
		pb.ptyOutMs = BigInt(rec.lastPtyOutMs || Date.now());
		pb.workerEmitMs = BigInt(Date.now());
	}
	noteHyperlinkSaturation(this, channelId, core, String(rec.sessionId));
	noteUnhandledSequences(rec, core);
	let repairOwed = false;
	if (next.frame.full) {
		rec.cell_emit = next.state;
		core.clearDirty();
		this.cellDirty.delete(channelId);
		clearStreamDeliveryDirty(state);
		if (!installStreamBaseline(this, channelId, state, pb)) {
			noteRejectedCellEmission(rec, "baseline_invalidated");
			return;
		}
	} else {
		const fanout = sendCellDeltaToSinks(this, channelId, pb);
		if (fanout.accepted === 0) {
			// Nobody took this seq, so the repair full re-uses it and the
			// receiver's sequence space stays contiguous.
			noteRejectedCellEmission(rec, "baseline_invalidated");
			this.pendingCellRepairs.add(channelId);
			invalidateStreamBaselines(this, state);
			this.installTerminalBaseline(asChannelId(channelId));
			return;
		}
		rec.cell_emit = next.state;
		core.clearDirty();
		this.cellDirty.delete(channelId);
		repairOwed = fanout.dropped > 0;
	}
	// Diagnostics observe the ACCEPTED frame only, and never advance cell_emit,
	// the core's dirty rows or the manager's dirty set.
	noteAcceptedCellEmission(rec, state, next.frame);
	rec.lastPtyOutMs = 0;
	if (isDiagEnabled()) {
		diag("cell.emit", {
			sid: String(rec.sessionId),
			stream_id: state.streamId,
			seq: next.frame.seq,
			base_seq: next.frame.baseSeq,
			full: next.frame.full,
			vp_rows: next.frame.viewportRows.length,
			result: aggregateStreamDelivery(this, state).snapshotPending ? "cursor" : "sent",
		});
	}
	// A sink that dropped the delta its siblings took owes a fresh baseline, and
	// the worker fold can no longer reproduce what every receiver holds. The
	// repair is stream-wide because one core yields one frame per tick.
	if (repairOwed) {
		noteRejectedCellEmission(rec, "baseline_invalidated");
		this.pendingCellRepairs.add(channelId);
		invalidateStreamBaselines(this, state);
		this.installTerminalBaseline(asChannelId(channelId));
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
