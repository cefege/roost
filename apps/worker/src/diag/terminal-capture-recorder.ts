// Mutable state of one armed terminal-incident recording: its lease, its
// segment chain, the emitted-frame fold, the core-sampling gates and the
// automatic-capture latch. Owned and mutated only by diag/terminal-capture.ts;
// bounded retention lives in diag/terminal-capture-pools.ts and projection into
// the immutable bundle shapes in diag/terminal-capture-worker-section.ts.
// Every bound comes from TERMINAL_CAPTURE_LIMITS.

import { randomUUID } from "node:crypto";
import type { CellGridFrame } from "@roost/protocol/cell";
import type { TerminalGeometry } from "@roost/protocol/viewport";
import {
	TERMINAL_CAPTURE_LIMITS,
	type TerminalCaptureFileRef,
	type TerminalCoverageReason,
	type TerminalWorkerCoreSampleRecord,
	type TerminalWorkerEmissionRecord,
	type TerminalWorkerRawRecord,
	type TerminalWorkerResizeOutcome,
	type TerminalWorkerSegment,
} from "@roost/protocol/terminal-capture";
import type { TerminalCaptureWorkerAck } from "./terminal-capture-ack.ts";
import {
	noteWorkerOmission,
	SEGMENT_POOL,
	SEGMENT_RECORD_BYTES,
	type MutableWorkerOmission,
	type RetainedWorkerRecord,
} from "./terminal-capture-pools.ts";

/** `closed_at_ms` is written when the next segment opens, so the chain a
 *  capture freezes says how long each generation was live. */
export interface MutableWorkerSegment {
	readonly segment_id: string;
	readonly stream_id: string;
	readonly grid_epoch: string;
	readonly core_incarnation: number;
	readonly opened_at_ms: number;
	closed_at_ms: number | null;
	readonly open_reason: TerminalWorkerSegment["open_reason"];
	readonly geometry: TerminalGeometry;
	readonly open_offset: string;
}

export interface MutableWorkerResizeRecord {
	readonly segment_id: string;
	readonly at_ms: number;
	readonly resize_seq: number;
	readonly install_offset: string;
	boundary_offset: string | null;
	readonly from: TerminalGeometry;
	readonly to: TerminalGeometry;
	outcome: TerminalWorkerResizeOutcome;
	readonly grid_epoch_before: string;
	grid_epoch_after: string | null;
	captured_bytes: number;
}

export interface MutableSamplingStats {
	sampled: number;
	skipped_interval: number;
	skipped_budget: number;
	skipped_grid: number;
	suppressed_until_ms: number | null;
	max_elapsed_us: number;
}

export interface MutableDropCounters {
	records: number;
	bytes: number;
	rows: number;
	raw_bytes: number;
	samples: number;
}

/** Bookkeeping that outlives one capture: the completed-capture results an RPC
 *  retry replays, the last WORKER-TRIGGERED saved file, and the manual-capture
 *  floor. Shared shape with the unarmed one-shot path, which has no lease but
 *  the same idempotency and rate obligations. `recent_worker_local` is only
 *  ever written by the emission-conflict path, so it can never report an
 *  operator- or browser-requested capture as a worker-detected incident. */
export interface CaptureLedger {
	readonly completed: Map<string, TerminalCaptureWorkerAck>;
	recent_worker_local: TerminalCaptureFileRef | null;
	last_manual_ms: number;
}

export function createCaptureLedger(): CaptureLedger {
	return {
		completed: new Map(),
		recent_worker_local: null,
		last_manual_ms: Number.NEGATIVE_INFINITY,
	};
}

export interface WorkerCaptureRecorder {
	readonly session_id: string;
	readonly worker_fp: string;
	recording_id: string;
	expires_at_ms: number;
	readonly armed_at_ms: number;
	/** Raw offset the recording started at. Non-zero means this core already
	 *  parsed bytes nobody retained, so exact parser replay is impossible. */
	readonly armed_offset: number;
	core_incarnation: number;
	segments: MutableWorkerSegment[];
	open_segment: MutableWorkerSegment | null;
	segment_key: string;
	last_epoch_base: string;
	raw: RetainedWorkerRecord<TerminalWorkerRawRecord>[];
	emissions: RetainedWorkerRecord<TerminalWorkerEmissionRecord>[];
	core_samples: RetainedWorkerRecord<TerminalWorkerCoreSampleRecord>[];
	resizes: MutableWorkerResizeRecord[];
	raw_bytes: number;
	cell_bytes: number;
	metadata_bytes: number;
	sampling: MutableSamplingStats;
	dropped: MutableDropCounters;
	omissions: Map<string, MutableWorkerOmission>;
	/** Viewport-only fold of the accepted emitted frames. Null until the next
	 *  accepted full re-establishes a baseline. */
	fold: CellGridFrame | null;
	fold_segment_id: string | null;
	fold_reasons: Set<TerminalCoverageReason>;
	raw_prefix_complete: boolean;
	last_sample_mono_ms: number;
	sample_suppressed_until_mono_ms: number;
	/** One automatic capture per (recording, stream, epoch, reason). */
	latches: Set<string>;
	occurrences: Map<string, number>;
	last_automatic_ms: number;
	capture_in_flight: boolean;
	readonly ledger: CaptureLedger;
}

export interface WorkerRecorderArming {
	readonly session_id: string;
	readonly worker_fp: string;
	readonly recording_id: string;
	readonly expires_at_ms: number;
	readonly at_ms: number;
	readonly head_seq: number;
}

export interface WorkerSegmentRequest {
	readonly stream_id: string;
	readonly stream_version: number;
	readonly grid_epoch: string;
	readonly grid_epoch_base: string;
	readonly geometry: TerminalGeometry;
	readonly head_seq: number;
	readonly at_ms: number;
}

export function createWorkerCaptureRecorder(
	arming: WorkerRecorderArming,
): WorkerCaptureRecorder {
	return {
		session_id: arming.session_id,
		worker_fp: arming.worker_fp,
		recording_id: arming.recording_id,
		expires_at_ms: arming.expires_at_ms,
		armed_at_ms: arming.at_ms,
		armed_offset: arming.head_seq,
		core_incarnation: 1,
		segments: [],
		open_segment: null,
		segment_key: "",
		last_epoch_base: "",
		raw: [],
		emissions: [],
		core_samples: [],
		resizes: [],
		raw_bytes: 0,
		cell_bytes: 0,
		metadata_bytes: 0,
		sampling: {
			sampled: 0,
			skipped_interval: 0,
			skipped_budget: 0,
			skipped_grid: 0,
			suppressed_until_ms: null,
			max_elapsed_us: 0,
		},
		dropped: { records: 0, bytes: 0, rows: 0, raw_bytes: 0, samples: 0 },
		omissions: new Map(),
		fold: null,
		fold_segment_id: null,
		fold_reasons: new Set(),
		raw_prefix_complete: arming.head_seq === 0,
		last_sample_mono_ms: Number.NEGATIVE_INFINITY,
		sample_suppressed_until_mono_ms: Number.NEGATIVE_INFINITY,
		latches: new Set(),
		occurrences: new Map(),
		last_automatic_ms: Number.NEGATIVE_INFINITY,
		capture_in_flight: false,
		ledger: createCaptureLedger(),
	};
}

/** The segment covering (stream generation × core incarnation × stream ID ×
 *  epoch). A transition opens a NEW segment and leaves the preceding one in the
 *  chain until ordinary bounded eviction, so a resize cannot erase the evidence
 *  of its own defect. */
export function openWorkerSegment(
	recorder: WorkerCaptureRecorder,
	request: WorkerSegmentRequest,
): MutableWorkerSegment {
	const key = `${request.stream_version}\u0000${request.stream_id}\u0000${request.grid_epoch}`;
	const open = recorder.open_segment;
	if (open && recorder.segment_key === key) return open;

	let reason: TerminalWorkerSegment["open_reason"] = "armed";
	if (open) {
		reason = recorder.last_epoch_base !== request.grid_epoch_base
			? "core_rebuild"
			: open.stream_id !== request.stream_id
				? "stream_change"
				: "epoch_change";
		open.closed_at_ms = request.at_ms;
		if (reason === "core_rebuild") recorder.core_incarnation += 1;
	}

	const segment: MutableWorkerSegment = {
		segment_id: randomUUID(),
		stream_id: request.stream_id,
		grid_epoch: request.grid_epoch,
		core_incarnation: recorder.core_incarnation,
		opened_at_ms: request.at_ms,
		closed_at_ms: null,
		open_reason: reason,
		geometry: { cols: request.geometry.cols, rows: request.geometry.rows },
		open_offset: String(request.head_seq),
	};
	recorder.segments.push(segment);
	recorder.metadata_bytes += SEGMENT_RECORD_BYTES;
	while (
		recorder.segments.length > TERMINAL_CAPTURE_LIMITS.layerEntries
		&& recorder.segments.length > 1
	) {
		recorder.segments.shift();
		recorder.metadata_bytes -= SEGMENT_RECORD_BYTES;
		recorder.dropped.records += 1;
		noteWorkerOmission(recorder, SEGMENT_POOL, SEGMENT_RECORD_BYTES, null);
	}
	recorder.open_segment = segment;
	recorder.segment_key = key;
	recorder.last_epoch_base = request.grid_epoch_base;
	// A new generation cannot inherit the previous fold: its absolute rows and
	// sequence space are a different grid.
	if (open) invalidateWorkerFold(recorder, "baseline_invalidated");
	return segment;
}

/** A dropped or rejected delta, a stream change and an epoch change all leave
 *  the fold unable to reproduce the shipped screen; only the next accepted full
 *  re-establishes it. */
export function invalidateWorkerFold(
	recorder: WorkerCaptureRecorder,
	reason: TerminalCoverageReason,
): void {
	recorder.fold = null;
	recorder.fold_segment_id = null;
	recorder.fold_reasons.add(reason);
}

export type WorkerSampleGate = "sample" | "grid" | "budget" | "interval";

/** Which gate a fresh core scan passes or trips, and the counter it moves. A
 *  grid too large is never sampled; a scan that overran its budget suppresses
 *  the next second of scans. */
export function classifyCoreSampleGate(
	recorder: WorkerCaptureRecorder,
	nowMonoMs: number,
	cells: number,
): WorkerSampleGate {
	if (cells > TERMINAL_CAPTURE_LIMITS.coreSampleMaxCells) {
		recorder.sampling.skipped_grid += 1;
		return "grid";
	}
	if (nowMonoMs < recorder.sample_suppressed_until_mono_ms) {
		recorder.sampling.skipped_budget += 1;
		return "budget";
	}
	if (nowMonoMs - recorder.last_sample_mono_ms < TERMINAL_CAPTURE_LIMITS.coreSampleIntervalMs) {
		recorder.sampling.skipped_interval += 1;
		return "interval";
	}
	return "sample";
}

export function noteCoreSampleElapsed(
	recorder: WorkerCaptureRecorder,
	nowMonoMs: number,
	elapsedUs: number,
	atMs: number,
): void {
	recorder.sampling.sampled += 1;
	recorder.last_sample_mono_ms = nowMonoMs;
	if (elapsedUs > recorder.sampling.max_elapsed_us) {
		recorder.sampling.max_elapsed_us = elapsedUs;
	}
	if (elapsedUs <= TERMINAL_CAPTURE_LIMITS.coreSampleBudgetUs) return;
	recorder.sample_suppressed_until_mono_ms =
		nowMonoMs + TERMINAL_CAPTURE_LIMITS.coreSampleSuppressMs;
	recorder.sampling.suppressed_until_ms =
		atMs + TERMINAL_CAPTURE_LIMITS.coreSampleSuppressMs;
}

/** Acquire the automatic-capture latch. The per-identity latch collapses a
 *  repeating mismatch into ONE capture; the session-wide floor then bounds
 *  captures across identities, so a fresh epoch cannot buy a new capture. */
export function latchAutomaticCapture(
	recorder: WorkerCaptureRecorder,
	key: string,
	nowMs: number,
): boolean {
	const held = recorder.latches.has(key);
	recorder.occurrences.set(key, (recorder.occurrences.get(key) ?? 0) + 1);
	if (held) return false;
	if (nowMs - recorder.last_automatic_ms < TERMINAL_CAPTURE_LIMITS.automaticCooldownMs) {
		return false;
	}
	recorder.latches.add(key);
	recorder.last_automatic_ms = nowMs;
	return true;
}
