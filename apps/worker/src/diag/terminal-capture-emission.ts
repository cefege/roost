// The accepted-emission tap: retain the exact frame the worker shipped, advance
// the viewport-only fold, sample a fresh core scan at that emission's own
// generation and sequence, and compare the two. Called only through
// diag/terminal-capture.ts's guarded tap, from session-emit.ts::emitCellFrame
// AFTER a full's snapshot cursor installed or a delta's send was accepted.
//
// It may NEVER advance the live emission state, clear core dirty rows, or touch
// the manager's dirty set: a diagnostic that mutated delivery state would be
// the defect it is looking for.

import {
	canonicalViewOfFrame,
	compareCanonicalViews,
	type TerminalCanonicalDifference,
	type TerminalWorkerComparison,
} from "@roost/shared/terminal-capture";
import {
	cellGridEpoch,
	cloneCellGridFrame,
	foldCellDeltaBatch,
	gridToCellFrame,
	normalizeCellGridFrame,
	scrollbackOrigin,
	type CellGridFrame,
} from "@roost/shared/cell";
import type { SessionRecord } from "../session-record.ts";
import type { TerminalStreamState } from "../session-terminal-state.ts";
import { monoNowMs } from "../util/mono.ts";
import {
	approximateCellFrameBytes,
	CORE_SAMPLE_POOL,
	EMISSION_POOL,
	retainWorkerRecord,
} from "./terminal-capture-pools.ts";
import {
	classifyCoreSampleGate,
	invalidateWorkerFold,
	noteCoreSampleElapsed,
	openWorkerSegment,
	type WorkerCaptureRecorder,
	type WorkerSampleGate,
} from "./terminal-capture-recorder.ts";

/** A proven disagreement between the shipped fold and a fresh core scan, at one
 *  exact generation and sequence. The caller latches and schedules; this module
 *  never writes a file. */
export interface WorkerEmissionConflict {
	readonly stream_id: string;
	readonly grid_epoch: string;
	readonly seq: string;
	readonly difference: TerminalCanonicalDifference;
}

export function recordAcceptedEmission(
	recorder: WorkerCaptureRecorder,
	record: SessionRecord,
	stream: TerminalStreamState,
	frame: CellGridFrame,
): WorkerEmissionConflict | null {
	const atMs = Date.now();
	const segment = openWorkerSegment(recorder, {
		stream_id: frame.streamId,
		stream_version: stream.version,
		grid_epoch: frame.gridEpoch,
		grid_epoch_base: record.cell_emit.gridEpochBase,
		geometry: { cols: frame.cols, rows: frame.rows },
		head_seq: record.head_seq,
		at_ms: atMs,
	});
	const identity = {
		stream_id: frame.streamId,
		grid_epoch: frame.gridEpoch,
		seq: String(frame.seq),
		base_seq: frame.full ? null : String(frame.baseSeq),
		cols: frame.cols,
		rows: frame.rows,
	};

	const advanced = advanceFold(recorder, segment.segment_id, frame);
	const sample = advanced
		? sampleFreshCore(recorder, record, frame, atMs, segment.segment_id)
		: null;
	const comparison: TerminalWorkerComparison = !advanced
		? "baseline_invalid"
		: sample === null || sample.gate === "interval"
			? "unsampled"
			: sample.gate !== "sample"
				? "budget_skipped"
				: sample.difference
					? "different"
					: "equal";

	const emissionBytes = approximateCellFrameBytes(frame);
	const retained = retainWorkerRecord(
		recorder,
		recorder.emissions,
		EMISSION_POOL,
		{
			segment_id: segment.segment_id,
			emitted_at_ms: atMs,
			stream: identity,
			full: frame.full,
			frame,
			comparison,
			difference: sample?.difference ?? null,
		},
		emissionBytes,
	);
	if (!retained) {
		// One frame alone over the cell budget: the segment is unavailable rather
		// than retained unbounded, and emission is not held up for it.
		invalidateWorkerFold(recorder, "frame_over_budget");
	}
	if (!sample?.difference) return null;
	return {
		stream_id: frame.streamId,
		grid_epoch: frame.gridEpoch,
		seq: String(frame.seq),
		difference: sample.difference,
	};
}

/** A full establishes the fold; a delta advances it through the production
 *  folding functions. `foldCellDeltaBatch` clones both sides, so the frame this
 *  recorder already retained as evidence is never rewritten by a later fold. */
function advanceFold(
	recorder: WorkerCaptureRecorder,
	segmentId: string,
	frame: CellGridFrame,
): boolean {
	if (frame.full) {
		const baseline = normalizeCellGridFrame(cloneCellGridFrame(frame));
		if (canonicalViewOfFrame(baseline) === null) {
			invalidateWorkerFold(recorder, "baseline_invalidated");
			return false;
		}
		recorder.fold = baseline;
		recorder.fold_segment_id = segmentId;
		return true;
	}
	if (recorder.fold === null || recorder.fold_segment_id !== segmentId) return false;
	const batch = foldCellDeltaBatch(recorder.fold, [frame]);
	if (!batch) {
		invalidateWorkerFold(recorder, "baseline_invalidated");
		return false;
	}
	recorder.fold = normalizeCellGridFrame(batch.frame);
	return true;
}

interface CoreSampleOutcome {
	readonly gate: WorkerSampleGate;
	readonly difference: TerminalCanonicalDifference | null;
}

/** One fresh viewport-only core scan at this emission's exact generation and
 *  sequence, never enumerating history: `gridToCellFrame` with `tailRows = 0`
 *  is the authoritative viewport-only contract. */
function sampleFreshCore(
	recorder: WorkerCaptureRecorder,
	record: SessionRecord,
	frame: CellGridFrame,
	atMs: number,
	segmentId: string,
): CoreSampleOutcome | null {
	const fold = recorder.fold;
	if (!fold) return null;
	const monoBefore = monoNowMs();
	const gate = classifyCoreSampleGate(recorder, monoBefore, frame.cols * frame.rows);
	if (gate !== "sample") return { gate, difference: null };
	const core = record.wtermCore;
	let coreFrame: CellGridFrame;
	try {
		coreFrame = gridToCellFrame(
			core,
			frame.seq,
			cellGridEpoch(record.cell_emit),
			frame.streamId,
			0,
			scrollbackOrigin(core, record.cell_emit),
		);
	} catch {
		invalidateWorkerFold(recorder, "core_export_unavailable");
		return { gate: "budget", difference: null };
	}
	const elapsedUs = Math.round((monoNowMs() - monoBefore) * 1000);
	noteCoreSampleElapsed(recorder, monoBefore, elapsedUs, atMs);

	const coreView = canonicalViewOfFrame(coreFrame);
	const foldView = canonicalViewOfFrame(fold);
	if (!coreView || !foldView) {
		invalidateWorkerFold(recorder, "baseline_invalidated");
		return { gate: "budget", difference: null };
	}
	const difference = compareCanonicalViews(foldView, coreView);
	const bytes = approximateCellFrameBytes(coreFrame) + approximateCellFrameBytes(fold);
	retainWorkerRecord(
		recorder,
		recorder.core_samples,
		CORE_SAMPLE_POOL,
		{
			segment_id: segmentId,
			sampled_at_ms: atMs,
			stream: {
				stream_id: frame.streamId,
				grid_epoch: frame.gridEpoch,
				seq: String(frame.seq),
				base_seq: null,
				cols: coreFrame.cols,
				rows: coreFrame.rows,
			},
			elapsed_us: elapsedUs,
			core_frame: coreFrame,
			// The fold object is replaced, never mutated, so retaining it here
			// freezes exactly the state that was compared.
			fold_frame: fold,
			comparison: difference ? "different" : "equal",
			difference,
		},
		bytes,
	);
	return { gate, difference };
}
