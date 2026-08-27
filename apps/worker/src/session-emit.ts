// Upstream PTY-byte and cell-grid emission. Called with a SessionManager `this`
// (see wrappers in session-manager.ts). The raw-metadata staging lane lives in
// session-raw-metadata.ts and the synchronized-output holds in
// session-sync-output.ts; this file owns the rate governor and frame emitter.

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
	CELL_EMIT_COALESCE_MS,
	SYNC_OUTPUT_MAX_MS,
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
import { disposeRawMetadataState } from "./session-raw-metadata.ts";
import { releaseSyncOutputHold, syncOutputAction } from "./session-sync-output.ts";

// Timer-map values across the cell/raw-metadata governors use NULL for the
// armed-leading-edge state (a one-shot microtask, nothing cancellable): a real
// Timeout means the trailing coalesce is armed, absence means nothing is. The
// old `-1 as unknown as NodeJS.Timeout` sentinel made every reader prove it
// knew the fake; null plus explicit has-checks keeps the type honest.

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
	// Raw-metadata staging and synchronized-output holds own their own teardown;
	// the remaining per-channel flags are plain Set/Map drops.
	disposeRawMetadataState(this, channelId);
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
			if (armed !== undefined && armed !== null) {
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
		if (!promoteInputEcho || pending === null) return;
		clearTimeout(pending);
		this.cellEmitTimers.delete(channelId);
	}

	this.cellEmitTimers.set(channelId, null);
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
		if (pending !== null) clearTimeout(pending);
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
