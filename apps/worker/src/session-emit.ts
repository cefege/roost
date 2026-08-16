// Upstream PTY-byte and cell-grid emission. Called with a SessionManager `this`
// (see wrappers in session-manager.ts).

import type { SessionManager } from "./session-manager.ts";
import type { ChannelId } from "@roost/shared";
import { log, diag, isDiagEnabled, signal, asChannelId } from "@roost/shared";
import { DIR_FROM_PTY } from "@roost/shared/wire";
import { nextCellFrame, SB_SNAPSHOT_HISTORY_ROWS } from "@roost/shared/cell";
import { cellFrameToProto } from "@roost/shared/cell/cell-proto";
import type { MuxChannelCallbacks } from "./keeper/multiplexed-client.ts";
import type { TransportSendResult } from "./transport/CoordLink-types.ts";
import {
	RECENTLY_CLOSED_TTL_MS,
	KEEPER_DEGRADED_WINDOW_MS,
	KEEPER_DEGRADED_THRESHOLD,
	CELL_EMIT_COALESCE_MS,
	RAW_METADATA_COALESCE_MS,
	RAW_METADATA_CHANNEL_CAP_BYTES,
	RAW_METADATA_AGGREGATE_CAP_BYTES,
} from "./session-constants.ts";


// Leading-edge sentinel shared by the cell and raw-metadata governors.
const LEADING_SENTINEL = -1 as unknown as NodeJS.Timeout;

// phase-ssb7: emitScrollbackMark + DIR_SCROLLBACK_MARK deleted.
// Splice ordering is now per-byte end_seq on each FROM_PTY frame
// (attachOutputClient.onOutput below). See CLAUDE.md L11 "scrollback
// seam torn" row.

/** Ingest one PTY chunk. All scrollback/grid consumers run synchronously
 * before the pooled keeper buffer can be reused. Raw metadata gets one
 * deliberate defensive copy because its send is deferred. */
export function emitUpstreamChunk(this: SessionManager, channelId: number, chunk: Buffer): void {
	const postResize = this.postResizeOutput.get(channelId);
	if (postResize) {
		// Keeper/socket buffers are pooled; own bytes retained across the async
		// core rebuild and preserve post-boundary order.
		postResize.push(Buffer.from(chunk));
		return;
	}
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
	const endSeq = this.appendScrollback(channelId, chunk);
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
	// Consume exactly once on the first returning chunk, even when no viewer is
	// attached. With a viewer, this promotes the echo past an armed trailing
	// timer; later output remains on the ordinary 16 ms cadence.
	const promoteInputEcho = this.inputSensitiveChannels.delete(channelId);
	if (this._hasActiveViewer(channelId)) {
		this._scheduleCellEmit(channelId, promoteInputEcho);
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
	}, RAW_METADATA_COALESCE_MS);
	this.rawMetadataTimers.set(channelId, timer);
}

export function flushPendingCellRepairs(this: SessionManager): void {
	for (const channelId of this.pendingCellRepairs) {
		if (!this.sessions.has(channelId)) {
			this.pendingCellRepairs.delete(channelId);
			continue;
		}
		if (this._hasActiveViewer(channelId)) this.emitCellFrame(channelId, true);
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
	this.cellDirty.delete(channelId);
}


/** Is any live viewer claiming this channel? Withdrawn viewers are removed
 *  (deferred withdraw) and crashed ones reaped at VIEWPORT_CLAIM_TTL_MS, so a
 *  non-empty claim set = a real watcher. Drives B's "don't emit to nobody". */
export function _hasActiveViewer(this: SessionManager, channelId: number): boolean {
	return (this.viewportClaims.get(channelId)?.size ?? 0) > 0;
}

/** Re-emit one authoritative full frame for every live claimed session after
 *  the worker→coordinator channel map has been re-primed by helloAck. */
export function resnapshotClaimedSessions(this: SessionManager): void {
	for (const [channelId, claims] of this.viewportClaims) {
		if (claims.size === 0 || !this.sessions.has(channelId)) continue;
		this.emitCellSnapshot(asChannelId(channelId));
	}
}

/** Rate governor: leading-edge cell emit plus trailing coalesce. A single
 * input-sensitive return chunk may replace an armed trailing timer with the
 * existing leading microtask; the governor re-arms from that promoted echo. */
export function _scheduleCellEmit(
	this: SessionManager,
	channelId: number,
	promoteInputEcho = false,
): void {
	if (this.cellEmissionGates.has(channelId)) {
		this.cellDirty.add(channelId);
		return;
	}
	if (this.pendingCellRepairs.has(channelId)) {
		this.cellDirty.add(channelId);
		return;
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

/** R11. Emit a cell frame upstream for `channelId`. Full frame on
 *  first emit / reframe (dims change, scrollback shrink, rebuild) or when
 *  forced (fresh viewer attach); delta from core.isDirtyRow otherwise.
 *  Full frames carry the current viewport and zero historical rows; retained
 *  history stays addressable through scrollbackTotal/sbBase and the explicit
 *  get-scrollback-cells path. clearDirty() AFTER reading so the next delta
 *  carries only new changes — the worker's wtermCore dirty bits have no
 *  other consumer. */
export function emitCellFrame(this: SessionManager, channelId: number, force: boolean): void {
	if (this.cellEmissionGates.has(channelId)) {
		this.cellDirty.add(channelId);
		return;
	}
	const send = this.sendCellGridUpstream;
	if (!send) return;
	const rec = this.sessions.get(channelId);
	if (!rec) return;
	// The live paths create a terminal core before registration. Keep this
	// narrow for teardown races and sparse test fixtures.
	const core = rec.wtermCore;
	if (!core) return;
	const pending = this.cellEmitTimers.get(channelId);
	if (pending !== undefined) {
		if (pending !== LEADING_SENTINEL) clearTimeout(pending);
		this.cellEmitTimers.delete(channelId);
	}
	const repair = this.pendingCellRepairs.has(channelId);
	const { frame, state } = nextCellFrame(
		core,
		rec.cell_emit,
		force || repair,
		SB_SNAPSHOT_HISTORY_ROWS,
	);
	// session_id left empty: coord stamps it from the channel→session map.
	const pb = cellFrameToProto(frame, "");
	pb.ptyOutMs = BigInt(rec.lastPtyOutMs || Date.now());
	pb.workerEmitMs = BigInt(Date.now());
	let result: TransportSendResult;
	try {
		result = send(channelId, pb) ?? "sent";
	} catch (error) {
		log.warn("session-manager", "cell_sink_throw", {
			channelId,
			error: error instanceof Error ? error.message : String(error),
		});
		result = "dropped";
	}
	if (result === "dropped") {
		// Do not advance the model watermark or clear dirty rows. A writable
		// edge will regenerate one full frame from the current canonical core.
		this.pendingCellRepairs.add(channelId);
		diag("transport.frame_dropped", {
			reason: "cell_sink_drop",
			kind: "cellGrid",
			channel_id: channelId,
			seq: frame.seq,
		});
		return;
	}
	rec.cell_emit = state;
	core.clearDirty();
	this.cellDirty.delete(channelId);
	this.pendingCellRepairs.delete(channelId);
	rec.lastPtyOutMs = 0;
	if (isDiagEnabled()) {
		diag("cell.emit", {
			sid: String(rec.sessionId),
			seq: frame.seq,
			full: frame.full,
			vp_rows: frame.viewportRows.length,
			sb_append: frame.scrollbackAppend.length,
			sb_rows: frame.scrollbackRows.length,
			sb_base: frame.sbBase,
			cursor_row: frame.cursorRow,
			cursor_col: frame.cursorCol,
			cursor_vis: frame.cursorVisible,
			alt: frame.altScreen,
			result,
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
