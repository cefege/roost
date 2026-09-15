// Freezes one armed (or unarmed) session's worker evidence into the immutable
// TerminalWorkerSection plus its coverage report. Called synchronously by
// diag/terminal-capture.ts before any await, so the state it records is the
// state at the trigger rather than the state after the repair.
// Reads the authoritative grid through @roost/shared/cell's existing readers;
// it never calls a keeper history RPC, and a display-cell snapshot is never
// treated as a parser checkpoint, because it is not parser state.

import { randomUUID } from "node:crypto";
import { readScrollbackRangeCells, scrollbackOrigin, type CellRow } from "@roost/shared/cell";
import { ROOST_ARTIFACT_VERSION, ROOST_BUILD_SHA } from "@roost/shared/build-identity";
import {
	TERMINAL_CAPTURE_LIMITS,
	type TerminalCaptureCoverage,
	type TerminalCaptureCoverageReport,
	type TerminalCaptureHistoryRange,
	type TerminalCaptureOmission,
	type TerminalCaptureStreamIdentity,
	type TerminalCoverageReason,
	type TerminalWorkerSection,
} from "@roost/shared/terminal-capture";
import type { SessionRecord } from "../session-record.ts";
import type { TerminalStreamState } from "../session-terminal-state.ts";
import { snapshotByteCapture } from "./byte-capture.ts";
import { workerOmissions } from "./terminal-capture-pools.ts";
import type { WorkerCaptureRecorder } from "./terminal-capture-recorder.ts";

/** Minted once per worker process so a bundle written after a restart is
 *  distinguishable from one written before it. */
const WORKER_PROCESS_ID = randomUUID();

export interface WorkerHistoryRequest {
	readonly start: number;
	readonly end: number;
}

export interface WorkerSectionRequest {
	readonly session_id: string;
	readonly worker_fp: string;
	readonly recorder: WorkerCaptureRecorder | null;
	readonly record: SessionRecord | undefined;
	readonly stream: TerminalStreamState | undefined;
	/** Absolute history rows the browser evidence named, end exclusive. */
	readonly history_ranges: readonly WorkerHistoryRequest[];
	readonly captured_at_ms: number;
}

export interface FrozenWorkerSection {
	readonly section: TerminalWorkerSection;
	readonly coverage: TerminalCaptureCoverageReport;
}

export function freezeWorkerSection(request: WorkerSectionRequest): FrozenWorkerSection {
	const { recorder, record, stream } = request;
	const omissions: TerminalCaptureOmission[] = recorder ? workerOmissions(recorder) : [];
	const segmentIds = new Set(recorder?.segments.map((segment) => segment.segment_id) ?? []);
	const grid = readGridEvidence(request, omissions);

	const emissions = recorder
		? recorder.emissions
			.filter((entry) => segmentIds.has(entry.record.segment_id))
			.map((entry) => entry.record)
		: [];
	const coreSamples = recorder
		? recorder.core_samples
			.filter((entry) => segmentIds.has(entry.record.segment_id))
			.map((entry) => entry.record)
		: [];
	const resizes = recorder
		? recorder.resizes.filter((entry) => segmentIds.has(entry.segment_id))
		: [];
	const raw = recorder
		? recorder.raw
			.filter((entry) => segmentIds.has(entry.record.segment_id))
			.map((entry) => entry.record)
		: [];
	// A record orphaned by segment eviction is unorderable, so it is dropped and
	// named rather than shipped as replayable evidence.
	const orphaned = recorder
		? (recorder.emissions.length - emissions.length)
			+ (recorder.core_samples.length - coreSamples.length)
			+ (recorder.raw.length - raw.length)
			+ (recorder.resizes.length - resizes.length)
		: 0;
	if (orphaned > 0) {
		omissions.push({
			kind: "records",
			name: "worker.segment_orphans",
			reason: "segment_evicted",
			dropped_count: orphaned,
			dropped_bytes: 0,
			range: null,
		});
	}

	const byteCapture = raw.length === 0 ? snapshotByteCapture(request.session_id) : null;
	const section: TerminalWorkerSection = {
		layer: "worker",
		captured_at_ms: request.captured_at_ms,
		process: {
			layer: "worker",
			process_id: WORKER_PROCESS_ID,
			git_sha: ROOST_BUILD_SHA,
			artifact_version: ROOST_ARTIFACT_VERSION,
			wasm_identity: null,
			worker_fp: request.worker_fp,
			viewer_id: null,
			user_agent: null,
		},
		stream: streamIdentityOf(record, stream),
		geometry: grid.readable ? { cols: grid.cols, rows: grid.rows } : null,
		dropped: recorder
			? { ...recorder.dropped }
			: { records: 0, bytes: 0, rows: 0, raw_bytes: 0, samples: 0 },
		omissions,
		segments: recorder?.segments.map((segment) => ({ ...segment })) ?? [],
		emissions,
		core_samples: coreSamples,
		sampling: recorder
			? { ...recorder.sampling }
			: {
				sampled: 0,
				skipped_interval: 0,
				skipped_budget: 0,
				skipped_grid: 0,
				suppressed_until_ms: null,
				max_elapsed_us: 0,
			},
		resizes: resizes.map((resize) => ({ ...resize })),
		raw,
		byte_capture: byteCapture,
		core_scrollback_tail: grid.tail,
		history_rows: grid.rows_evidence,
		history_ranges: grid.ranges,
		scrollback_total: grid.total,
		scrollback_origin: String(grid.origin),
	};
	return { section, coverage: coverageOf(request, section) };
}

interface GridEvidence {
	/** False when this session has no record here, or its core threw: geometry
	 *  is then reported absent rather than as a plausible-looking 0×0 grid. */
	readonly readable: boolean;
	readonly cols: number;
	readonly rows: number;
	readonly origin: number;
	readonly total: number;
	readonly tail: CellRow[];
	readonly rows_evidence: CellRow[];
	readonly ranges: TerminalCaptureHistoryRange[];
}

const UNREADABLE_GRID: GridEvidence = {
	readable: false,
	cols: 0,
	rows: 0,
	origin: 0,
	total: 0,
	tail: [],
	rows_evidence: [],
	ranges: [],
};

/** One bounded read of the live grid, OUTSIDE any per-emission loop. A trapped
 *  core can throw here; that is recorded as missing evidence rather than
 *  replaced with a plausible-looking empty grid. */
function readGridEvidence(
	request: WorkerSectionRequest,
	omissions: TerminalCaptureOmission[],
): GridEvidence {
	const record = request.record;
	if (!record) {
		return UNREADABLE_GRID;
	}
	const core = record.wtermCore;
	try {
		const origin = scrollbackOrigin(core, record.cell_emit);
		const total = origin + core.getScrollbackCount();
		const tailStart = Math.max(origin, total - TERMINAL_CAPTURE_LIMITS.coreScrollbackTailRows);
		const tail = readScrollbackRangeCells(core, tailStart, total, origin);
		const history = readHistoryRanges(core, origin, total, request.history_ranges);
		return {
			readable: true,
			cols: core.getCols(),
			rows: core.getRows(),
			origin,
			total,
			tail,
			rows_evidence: history.rows,
			ranges: history.ranges,
		};
	} catch {
		omissions.push({
			kind: "rows",
			name: "worker.history_rows",
			reason: "core_export_unavailable",
			dropped_count: 0,
			dropped_bytes: 0,
			range: null,
		});
		return UNREADABLE_GRID;
	}
}

interface HistoryEvidence {
	readonly rows: CellRow[];
	readonly ranges: TerminalCaptureHistoryRange[];
}

function readHistoryRanges(
	core: SessionRecord["wtermCore"],
	origin: number,
	total: number,
	requested: readonly WorkerHistoryRequest[],
): HistoryEvidence {
	const rows: CellRow[] = [];
	const ranges: TerminalCaptureHistoryRange[] = [];
	let budget = TERMINAL_CAPTURE_LIMITS.captureHistoryRows;
	const wanted = requested.length > 0
		? [...requested].sort((left, right) => left.start - right.start)
		: [{
			start: Math.max(origin, total - TERMINAL_CAPTURE_LIMITS.captureHistoryRows),
			end: total,
		}];
	for (const range of wanted) {
		// `worker.history_ranges` is validated against layerEntries; stop rather
		// than assemble a bundle the write-side gate would then refuse.
		if (ranges.length >= TERMINAL_CAPTURE_LIMITS.layerEntries - 2) break;
		if (range.start < origin) {
			ranges.push({
				start: String(range.start),
				end: String(Math.min(range.end, origin)),
				status: "evicted",
				rows: 0,
			});
		}
		if (range.end > total) {
			ranges.push({
				start: String(Math.max(range.start, total)),
				end: String(range.end),
				status: "unavailable",
				rows: 0,
			});
		}
		const start = Math.max(range.start, origin);
		const end = Math.min(range.end, total, start + budget);
		if (end <= start || budget <= 0) continue;
		const page = readScrollbackRangeCells(core, start, end, origin);
		for (const row of page) rows.push(row);
		budget -= page.length;
		ranges.push({
			start: String(start),
			end: String(end),
			status: "present",
			rows: page.length,
		});
	}
	return { rows, ranges };
}

function streamIdentityOf(
	record: SessionRecord | undefined,
	stream: TerminalStreamState | undefined,
): TerminalCaptureStreamIdentity | null {
	if (!record || !stream) return null;
	return {
		stream_id: stream.streamId,
		grid_epoch: `${record.cell_emit.gridEpochBase}:${record.cell_emit.gridEpochRevision}`,
		seq: String(record.cell_emit.seq),
		base_seq: null,
		cols: stream.cols > 0 ? stream.cols : record.wtermCore.getCols(),
		rows: stream.rows > 0 ? stream.rows : record.wtermCore.getRows(),
	};
}

function coverageOf(
	request: WorkerSectionRequest,
	section: TerminalWorkerSection,
): TerminalCaptureCoverageReport {
	const recorder = request.recorder;
	const cell = cellReplayCoverage(recorder, section);
	const core = coreReplayCoverage(recorder, section);
	const comparison = coreComparisonCoverage(recorder);
	return {
		cell_replay: cell.coverage,
		cell_replay_reasons: cell.reasons,
		core_replay: core.coverage,
		core_replay_reasons: core.reasons,
		core_comparison: comparison.coverage,
		core_comparison_reasons: comparison.reasons,
	};
}

interface CoverageVerdict {
	readonly coverage: TerminalCaptureCoverage;
	readonly reasons: readonly TerminalCoverageReason[];
}

function cellReplayCoverage(
	recorder: WorkerCaptureRecorder | null,
	section: TerminalWorkerSection,
): CoverageVerdict {
	if (!recorder) return { coverage: "unavailable", reasons: ["layer_unavailable"] };
	if (section.emissions.length === 0) {
		return { coverage: "unavailable", reasons: ["layer_unavailable"] };
	}
	const reasons = [...recorder.fold_reasons];
	if (recorder.fold === null && reasons.length === 0) reasons.push("baseline_invalidated");
	if (recorder.dropped.records > 0 && !reasons.includes("segment_evicted")) {
		reasons.push("segment_evicted");
	}
	if (reasons.length === 0) return { coverage: "complete", reasons: ["complete"] };
	return { coverage: "partial", reasons };
}

/** Complete ONLY with proven continuous output and geometry from this exact
 *  core's initialization. A retained tail without the prefix that produced the
 *  current parser state cannot attribute anything to the core. */
function coreReplayCoverage(
	recorder: WorkerCaptureRecorder | null,
	section: TerminalWorkerSection,
): CoverageVerdict {
	if (!recorder) return { coverage: "unavailable", reasons: ["layer_unavailable"] };
	const reasons: TerminalCoverageReason[] = [];
	// Armed mid-session, or the retained window no longer reaches the first
	// retained byte: either way the bytes that produced the current parser state
	// are not all here.
	if (recorder.armed_offset > 0 || section.raw.length === 0) {
		reasons.push("missing_initial_prefix");
	}
	if (!recorder.raw_prefix_complete) reasons.push("raw_prefix_evicted");
	if (section.resizes.some((resize) => resize.boundary_offset === null)) {
		reasons.push("missing_resize_boundary");
	}
	if (reasons.length === 0) return { coverage: "complete", reasons: ["complete"] };
	return { coverage: "partial", reasons };
}

/** Sampled equality proves only its own checkpoints, so the unsampled interval
 *  between two scans is reported as missing coverage rather than agreement. */
function coreComparisonCoverage(recorder: WorkerCaptureRecorder | null): CoverageVerdict {
	if (!recorder) return { coverage: "unavailable", reasons: ["layer_unavailable"] };
	const reasons: TerminalCoverageReason[] = [];
	if (recorder.sampling.skipped_grid > 0) reasons.push("grid_budget_exceeded");
	if (recorder.sampling.skipped_budget > 0) reasons.push("sample_budget_exceeded");
	if (recorder.sampling.skipped_interval > 0) reasons.push("core_export_unavailable");
	if (recorder.sampling.sampled === 0) {
		return {
			coverage: "unavailable",
			reasons: reasons.length > 0 ? reasons : ["layer_unavailable"],
		};
	}
	if (reasons.length === 0) return { coverage: "complete", reasons: ["complete"] };
	return { coverage: "partial", reasons };
}
