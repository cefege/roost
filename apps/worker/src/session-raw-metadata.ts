// Coordinator-only raw-metadata lane: copied PTY bytes remain bounded and
// source-ordered per channel while one manager-owned dispatcher drains them.
// Its ready ring rotates one head frame per channel and uses one global wake,
// so metadata never becomes a higher-priority terminal-data path than cells.
// Called from session-emit's emitUpstreamChunk; state lives on SessionManager.

import type { SessionManager } from "./session-manager.ts";
import { diag, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { DIR_FROM_PTY } from "@roost/shared/wire";
import {
	CELL_EMIT_COALESCE_MS,
	RAW_METADATA_AGGREGATE_CAP_BYTES,
	RAW_METADATA_CHANNEL_CAP_BYTES,
} from "./session-constants.ts";
import type { TransportSendResult } from "./transport/coord-link-types.ts";
import { monoNowMs } from "./util/mono.ts";

export const RAW_METADATA_DISPATCH_FRAME_BUDGET = 32;
export const RAW_METADATA_DISPATCH_MAX_TURN_MS = 4;
const RAW_METADATA_RING_COMPACTION_MIN_HEAD = 64;

type RawMetadataDrainResult = "attempted" | "deferred" | "skipped";

interface RawMetadataFrame {
	endSeq: number;
	bytes: Uint8Array;
}

class RawMetadataFrameRing {
	#items: RawMetadataFrame[] = [];
	#head = 0;

	get length(): number {
		return this.#items.length - this.#head;
	}

	append(frame: RawMetadataFrame): void {
		this.#items.push(frame);
	}

	peek(): RawMetadataFrame | undefined {
		return this.#items[this.#head];
	}

	take(): RawMetadataFrame | undefined {
		const frame = this.#items[this.#head];
		if (!frame) return undefined;
		this.#head += 1;
		if (this.#head === this.#items.length) {
			this.clear();
		} else if (
			this.#head >= RAW_METADATA_RING_COMPACTION_MIN_HEAD &&
			this.#head * 2 >= this.#items.length
		) {
			// A hot channel can otherwise retain consumed slots indefinitely.
			this.#items = this.#items.slice(this.#head);
			this.#head = 0;
		}
		return frame;
	}

	clear(): void {
		this.#items.length = 0;
		this.#head = 0;
	}
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
	const channelBytes = queue?.bytes ?? 0;
	if (
		chunk.byteLength > RAW_METADATA_CHANNEL_CAP_BYTES ||
		channelBytes + chunk.byteLength > RAW_METADATA_CHANNEL_CAP_BYTES ||
		this.rawMetadataQueuedBytes + chunk.byteLength > RAW_METADATA_AGGREGATE_CAP_BYTES
	) {
		diag("transport.frame_dropped", {
			reason: "raw_metadata_stage_overflow",
			kind: "raw",
			channel_id: channelId,
			channel_bytes: channelBytes,
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
	if (!queue) {
		queue = { frames: new RawMetadataFrameRing(), bytes: 0 };
		this.rawMetadataQueues.set(channelId, queue);
	}
	const stableBytes = Uint8Array.from(chunk);
	queue.frames.append({ endSeq, bytes: stableBytes });
	queue.bytes += stableBytes.byteLength;
	this.rawMetadataQueuedBytes += stableBytes.byteLength;
	markRawMetadataReady(this, channelId);
	scheduleRawMetadataDispatch(this);
}

function markRawMetadataReady(mgr: SessionManager, channelId: number): void {
	mgr.rawMetadataReadyRing.add(channelId);
}

function takeRawMetadataReadyChannel(mgr: SessionManager): number | null {
	const next = mgr.rawMetadataReadyRing.values().next();
	if (next.done || next.value === undefined) return null;
	const channelId = next.value;
	mgr.rawMetadataReadyRing.delete(channelId);
	return channelId;
}

function scheduleRawMetadataDispatch(mgr: SessionManager): void {
	if (mgr.rawMetadataDispatching || mgr.rawMetadataWake) return;
	mgr.rawMetadataWake = { kind: "microtask" };
	queueMicrotask(() => {
		if (mgr.rawMetadataWake?.kind !== "microtask") return;
		mgr.rawMetadataWake = null;
		drainRawMetadata(mgr);
	});
}

// Keep one trailing window after a real drain so a subsequent PTY burst joins
// the lower-priority metadata cadence instead of racing a cell coalesce.
function armRawMetadataWake(
	mgr: SessionManager,
	keepTrailingWindow: boolean,
): void {
	if (
		(!keepTrailingWindow && mgr.rawMetadataReadyRing.size === 0) ||
		mgr.rawMetadataDispatching ||
		mgr.rawMetadataWake
	) return;
	let timer: NodeJS.Timeout;
	timer = setTimeout(() => {
		const wake = mgr.rawMetadataWake;
		if (wake?.kind !== "timer" || wake.timer !== timer) return;
		mgr.rawMetadataWake = null;
		drainRawMetadata(mgr);
	}, CELL_EMIT_COALESCE_MS);
	mgr.rawMetadataWake = { kind: "timer", timer };
}

function drainRawMetadata(mgr: SessionManager): void {
	if (mgr.rawMetadataDispatching) return;
	mgr.rawMetadataDispatching = true;
	const deadlineMs = monoNowMs() + RAW_METADATA_DISPATCH_MAX_TURN_MS;
	let frames = 0;
	try {
		while (
			frames < RAW_METADATA_DISPATCH_FRAME_BUDGET &&
			monoNowMs() < deadlineMs
		) {
			const channelId = takeRawMetadataReadyChannel(mgr);
			if (channelId === null) break;
			const result = drainRawMetadataHead(mgr, channelId);
			if (result === "deferred") break;
			if (result === "attempted") frames += 1;
		}
	} finally {
		mgr.rawMetadataDispatching = false;
	}
	armRawMetadataWake(mgr, frames > 0);
}

function drainRawMetadataHead(
	mgr: SessionManager,
	channelId: number,
): RawMetadataDrainResult {
	const queue = mgr.rawMetadataQueues.get(channelId);
	if (!queue || queue.frames.length === 0) {
		if (queue) mgr.rawMetadataQueues.delete(channelId);
		return "skipped";
	}
	if (!mgr.sessions.has(channelId)) {
		disposeRawMetadataState(mgr, channelId);
		return "skipped";
	}
	const send = mgr.sendBinaryUpstream;
	if (!send) {
		markRawMetadataReady(mgr, channelId);
		return "deferred";
	}
	const frame = queue.frames.peek();
	if (!frame) {
		disposeRawMetadataState(mgr, channelId);
		return "skipped";
	}
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
	if (mgr.rawMetadataQueues.get(channelId) !== queue) return "attempted";
	if (result === "dropped") {
		dropRawMetadataQueue(mgr, channelId, queue);
		return "attempted";
	}
	if (queue.frames.peek() !== frame) return "attempted";
	queue.frames.take();
	releaseRawMetadataBytes(mgr, frame.bytes.byteLength);
	queue.bytes -= frame.bytes.byteLength;
	log.debug("session-manager", "emit_upstream", {
		channelId,
		len: frame.bytes.byteLength,
		endSeq: frame.endSeq,
		result,
	});
	if (queue.frames.length === 0) {
		mgr.rawMetadataQueues.delete(channelId);
	} else {
		markRawMetadataReady(mgr, channelId);
	}
	return "attempted";
}

function dropRawMetadataQueue(
	mgr: SessionManager,
	channelId: number,
	queue: { frames: { readonly length: number; clear(): void }; bytes: number },
): void {
	const droppedFrames = queue.frames.length;
	const droppedBytes = queue.bytes;
	queue.frames.clear();
	queue.bytes = 0;
	releaseRawMetadataBytes(mgr, droppedBytes);
	mgr.rawMetadataQueues.delete(channelId);
	removeRawMetadataReady(mgr, channelId);
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
}

function releaseRawMetadataBytes(mgr: SessionManager, bytes: number): void {
	mgr.rawMetadataQueuedBytes -= bytes;
}

function removeRawMetadataReady(mgr: SessionManager, channelId: number): void {
	mgr.rawMetadataReadyRing.delete(channelId);
}

function cancelRawMetadataWakeWhenIdle(mgr: SessionManager): void {
	if (mgr.rawMetadataReadyRing.size !== 0) return;
	const wake = mgr.rawMetadataWake;
	if (wake?.kind !== "timer") return;
	clearTimeout(wake.timer);
	mgr.rawMetadataWake = null;
}

/** Drop one channel's staging state without canceling another channel's wake. */
export function disposeRawMetadataState(mgr: SessionManager, channelId: number): void {
	removeRawMetadataReady(mgr, channelId);
	const queue = mgr.rawMetadataQueues.get(channelId);
	if (queue) {
		mgr.rawMetadataQueues.delete(channelId);
		releaseRawMetadataBytes(mgr, queue.bytes);
	}
	cancelRawMetadataWakeWhenIdle(mgr);
}
