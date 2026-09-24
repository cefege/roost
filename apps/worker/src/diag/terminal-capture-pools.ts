// Bounded record pools for one armed terminal-incident recording: the byte
// budgets, the oldest-first eviction, and the omission ledger that names every
// evicted range. Called only by diag/terminal-capture-recorder.ts and
// diag/terminal-capture.ts; the recorder state shape it mutates is declared in
// diag/terminal-capture-recorder.ts (imported as types only, so this stays a
// leaf at runtime). Every ceiling comes from TERMINAL_CAPTURE_LIMITS.

import type { CellGridFrame, CellRow } from "@roost/protocol/cell";
import {
	TERMINAL_CAPTURE_LIMITS,
	type TerminalCaptureOmission,
	type TerminalCoverageReason,
} from "@roost/protocol/terminal-capture";
import type {
	MutableWorkerResizeRecord,
	WorkerCaptureRecorder,
} from "./terminal-capture-recorder.ts";

/** Flat-rate JSON cost of one record's scalar envelope. Retention is decided
 *  on the emission path, where measuring a frame by stringifying it would cost
 *  more than retaining it. */
const FRAME_BASE_BYTES = 384;
const ROW_BASE_BYTES = 24;
const SPAN_BASE_BYTES = 56;
const RAW_RECORD_BASE_BYTES = 160;
export const SEGMENT_RECORD_BYTES = 288;
export const RESIZE_RECORD_BYTES = 320;

export type WorkerBytePool = "raw" | "cell" | "metadata";

export interface WorkerPoolSpec {
	readonly name: string;
	readonly kind: TerminalCaptureOmission["kind"];
	readonly reason: TerminalCoverageReason;
	readonly budget: WorkerBytePool;
}

export interface WorkerOffsetRange {
	start: string;
	end: string;
}

export const RAW_POOL: WorkerPoolSpec = {
	name: "worker.raw",
	kind: "raw",
	reason: "raw_prefix_evicted",
	budget: "raw",
};
export const EMISSION_POOL: WorkerPoolSpec = {
	name: "worker.emissions",
	kind: "records",
	reason: "segment_evicted",
	budget: "cell",
};
export const CORE_SAMPLE_POOL: WorkerPoolSpec = {
	name: "worker.core_samples",
	kind: "sample",
	reason: "segment_evicted",
	budget: "cell",
};
export const RESIZE_POOL: WorkerPoolSpec = {
	name: "worker.resizes",
	kind: "records",
	reason: "missing_resize_boundary",
	budget: "metadata",
};
export const SEGMENT_POOL: WorkerPoolSpec = {
	name: "worker.segments",
	kind: "records",
	reason: "segment_evicted",
	budget: "metadata",
};

export interface RetainedWorkerRecord<T> {
	readonly record: T;
	readonly bytes: number;
	/** Absolute offsets this record covers, so eviction names exactly what the
	 *  bundle no longer contains. */
	readonly range: WorkerOffsetRange | null;
}

export interface MutableWorkerOmission {
	readonly kind: TerminalCaptureOmission["kind"];
	readonly name: string;
	readonly reason: TerminalCoverageReason;
	dropped_count: number;
	dropped_bytes: number;
	range: WorkerOffsetRange | null;
}

export function recorderRetainedBytes(recorder: WorkerCaptureRecorder): number {
	return recorder.raw_bytes + recorder.cell_bytes + recorder.metadata_bytes;
}

/** Approximate JSON cost of one frame: O(spans) rather than O(stringify), so
 *  the emission path can pay it inline and still enforce a megabyte budget. */
export function approximateCellFrameBytes(frame: CellGridFrame): number {
	return FRAME_BASE_BYTES
		+ approximateCellRowsBytes(frame.viewportRows)
		+ approximateCellRowsBytes(frame.scrollbackRows)
		+ approximateCellRowsBytes(frame.scrollbackAppend);
}

export function approximateCellRowsBytes(rows: readonly CellRow[]): number {
	let bytes = 0;
	for (const row of rows) {
		bytes += ROW_BASE_BYTES;
		for (const span of row.spans) {
			bytes += SPAN_BASE_BYTES + span.text.length
				+ (span.linkUri?.length ?? 0) + (span.linkKey?.length ?? 0);
		}
	}
	return bytes;
}

export function rawRecordBytes(byteLength: number): number {
	// base64 inflates by 4/3 and the bundle carries the encoded form.
	return RAW_RECORD_BASE_BYTES + Math.ceil(byteLength / 3) * 4;
}

/** Retain one record, then evict oldest-first until every entry and byte bound
 *  holds again. Returns false when the record ALONE exceeds its byte budget:
 *  the caller then marks the segment unavailable rather than retaining an
 *  unbounded object or stalling emission. */
export function retainWorkerRecord<T>(
	recorder: WorkerCaptureRecorder,
	pool: RetainedWorkerRecord<T>[],
	spec: WorkerPoolSpec,
	record: T,
	bytes: number,
	range: WorkerOffsetRange | null = null,
): boolean {
	if (bytes > budgetCap(spec.budget)) return false;
	pool.push({ record, bytes, range });
	addBudget(recorder, spec.budget, bytes);
	while (
		pool.length > 0
		&& (
			pool.length > TERMINAL_CAPTURE_LIMITS.layerEntries
			|| budgetUsed(recorder, spec.budget) > budgetCap(spec.budget)
			|| recorderRetainedBytes(recorder) > TERMINAL_CAPTURE_LIMITS.layerBytes
		)
	) {
		const victim = pool.shift();
		if (!victim) break;
		addBudget(recorder, spec.budget, -victim.bytes);
		recorder.dropped.records += 1;
		recorder.dropped.bytes += victim.bytes;
		if (spec.budget === "raw") {
			recorder.dropped.raw_bytes += victim.bytes;
			// The retained window no longer reaches the recording's first byte,
			// so exact parser replay from this core's init is off the table.
			recorder.raw_prefix_complete = false;
		}
		if (spec === CORE_SAMPLE_POOL) recorder.dropped.samples += 1;
		noteWorkerOmission(recorder, spec, victim.bytes, victim.range);
	}
	return true;
}

export function retainResizeRecord(
	recorder: WorkerCaptureRecorder,
	record: MutableWorkerResizeRecord,
): void {
	recorder.resizes.push(record);
	recorder.metadata_bytes += RESIZE_RECORD_BYTES;
	while (
		recorder.resizes.length > TERMINAL_CAPTURE_LIMITS.layerEntries
		|| recorder.metadata_bytes > TERMINAL_CAPTURE_LIMITS.metadataBytes
	) {
		if (recorder.resizes.shift() === undefined) break;
		recorder.metadata_bytes -= RESIZE_RECORD_BYTES;
		recorder.dropped.records += 1;
		recorder.dropped.bytes += RESIZE_RECORD_BYTES;
		noteWorkerOmission(recorder, RESIZE_POOL, RESIZE_RECORD_BYTES, null);
	}
}

/** Coalesced by (kind, name, reason) so the ledger stays bounded by the small
 *  number of things that can be dropped, not by how often they were. */
export function noteWorkerOmission(
	recorder: WorkerCaptureRecorder,
	spec: WorkerPoolSpec,
	bytes: number,
	range: WorkerOffsetRange | null,
): void {
	const key = `${spec.kind}|${spec.name}|${spec.reason}`;
	const existing = recorder.omissions.get(key);
	if (!existing) {
		recorder.omissions.set(key, {
			kind: spec.kind,
			name: spec.name,
			reason: spec.reason,
			dropped_count: 1,
			dropped_bytes: bytes,
			range: range ? { start: range.start, end: range.end } : null,
		});
		return;
	}
	existing.dropped_count += 1;
	existing.dropped_bytes += bytes;
	if (!range) return;
	if (!existing.range) {
		existing.range = { start: range.start, end: range.end };
		return;
	}
	if (BigInt(range.start) < BigInt(existing.range.start)) existing.range.start = range.start;
	if (BigInt(range.end) > BigInt(existing.range.end)) existing.range.end = range.end;
}

export function workerOmissions(
	recorder: WorkerCaptureRecorder,
): TerminalCaptureOmission[] {
	return [...recorder.omissions.values()].map((omission) => ({
		kind: omission.kind,
		name: omission.name,
		reason: omission.reason,
		dropped_count: omission.dropped_count,
		dropped_bytes: omission.dropped_bytes,
		range: omission.range ? { start: omission.range.start, end: omission.range.end } : null,
	}));
}

function budgetCap(pool: WorkerBytePool): number {
	return pool === "raw"
		? TERMINAL_CAPTURE_LIMITS.rawBytes
		: pool === "cell"
			? TERMINAL_CAPTURE_LIMITS.cellBytes
			: TERMINAL_CAPTURE_LIMITS.metadataBytes;
}

function budgetUsed(recorder: WorkerCaptureRecorder, pool: WorkerBytePool): number {
	return pool === "raw"
		? recorder.raw_bytes
		: pool === "cell"
			? recorder.cell_bytes
			: recorder.metadata_bytes;
}

function addBudget(
	recorder: WorkerCaptureRecorder,
	pool: WorkerBytePool,
	bytes: number,
): void {
	if (pool === "raw") recorder.raw_bytes += bytes;
	else if (pool === "cell") recorder.cell_bytes += bytes;
	else recorder.metadata_bytes += bytes;
}
