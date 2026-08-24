// Coordinator-only raw-metadata lane for the cell emitter: PTY bytes are
// staged per channel (bounded, copied) and drained on a leading microtask plus
// a trailing coalesce timer so ready cell frames always lead. Timer-map values
// use null for the armed-leading-edge state — a real Timeout means the
// trailing coalesce is armed; absence from the map means nothing is.
// Called from session-emit's emitUpstreamChunk; state lives on SessionManager.

import type { SessionManager } from "./session-manager.ts";
import { diag, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { DIR_FROM_PTY } from "@roost/shared/wire";
import type { TransportSendResult } from "./transport/coord-link-types.ts";
import {
	CELL_EMIT_COALESCE_MS,
	RAW_METADATA_AGGREGATE_CAP_BYTES,
	RAW_METADATA_CHANNEL_CAP_BYTES,
} from "./session-constants.ts";

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
	// null = leading edge armed (microtask pending), not a cancellable timer.
	this.rawMetadataTimers.set(channelId, null);
	queueMicrotask(() => {
		this.rawMetadataTimers.delete(channelId);
		if (!this.sessions.has(channelId)) {
			disposeRawMetadataState(this, channelId);
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
			disposeRawMetadataState(this, channelId);
			return;
		}
		const queue = this.rawMetadataQueues.get(channelId);
		if (!queue || queue.frames.length === 0) return;
		_flushRawMetadata.call(this, channelId);
		_armRawMetadata.call(this, channelId);
	}, CELL_EMIT_COALESCE_MS);
	this.rawMetadataTimers.set(channelId, timer);
}

/** Drop the per-channel staging state. Called from session-emit's
 * _disposeOutputState and from the drain paths above when the channel died
 * mid-flight. */
export function disposeRawMetadataState(mgr: SessionManager, channelId: number): void {
	const timer = mgr.rawMetadataTimers.get(channelId);
	if (timer !== undefined && timer !== null) clearTimeout(timer);
	mgr.rawMetadataTimers.delete(channelId);
	const queue = mgr.rawMetadataQueues.get(channelId);
	if (queue) mgr.rawMetadataQueuedBytes -= queue.bytes;
	mgr.rawMetadataQueues.delete(channelId);
}
