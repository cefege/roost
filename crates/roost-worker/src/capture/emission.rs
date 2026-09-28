//! The accepted-emission tap: retain the exact frame the worker shipped,
//! advance the viewport-only fold, sample a fresh core scan at that emission's
//! own generation and sequence, and compare the two. Ports `apps/worker/src/
//! diag/terminal-capture-emission.ts`; called only by `super::recorder`, from
//! the emitter AFTER a full installed or a delta was accepted. It never
//! advances emission state or clears a dirty row: that would be the defect.

use std::sync::Arc;
use std::time::Instant;

use roost_protocol::cell::{
    CellGridFrame, clone_cell_grid_frame, fold_cell_delta_batch, normalize_cell_grid_frame,
};
use roost_protocol::terminal_capture::bundle::{
    TerminalCaptureStreamIdentity, TerminalCoverageReason as Reason, TerminalWorkerComparison,
    TerminalWorkerCoreSampleRecord, TerminalWorkerEmissionRecord,
};
use roost_protocol::terminal_capture::view::{
    TerminalCanonicalDifference, canonical_view_of_frame, compare_canonical_views,
};
use roost_protocol::viewport::TerminalGeometry;
use roost_term::{grid_to_cell_frame, scrollback_origin};

use super::now_ms;
use super::pools::{
    CORE_SAMPLE_POOL, EMISSION_POOL, approximate_cell_frame_bytes, retain_worker_record,
};
use super::recorder_state::{SampleGate, SegmentRequest, WorkerRecorder};
use crate::session::types::SessionRecord;

/// A proven disagreement between the shipped fold and a fresh core scan, at
/// one exact generation and sequence. The caller latches and schedules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmissionConflict {
    pub stream_id: String,
    pub grid_epoch: String,
    pub seq: String,
    pub difference: TerminalCanonicalDifference,
}

/// Record one ACCEPTED frame. `None` unless a fresh core scan disagreed.
pub fn record_accepted_emission(
    recorder: &mut WorkerRecorder,
    record: &SessionRecord,
    frame: Arc<CellGridFrame>,
) -> Option<EmissionConflict> {
    let at_ms = now_ms();
    let opened = recorder.open_worker_segment(&SegmentRequest {
        stream_id: &frame.stream_id,
        grid_epoch: &frame.grid_epoch,
        grid_epoch_base: &record.cell_emit.grid_epoch_base,
        geometry: TerminalGeometry {
            cols: frame.cols,
            rows: frame.rows,
        },
        head_seq: record.head_seq,
        at_ms,
    });
    let segment_id = match opened {
        Ok(segment_id) => segment_id,
        Err(error) => {
            tracing::warn!(
                session_id = %recorder.session_id,
                %error,
                "an accepted frame found no segment to be filed under"
            );
            return None;
        }
    };
    let advanced = advance_fold(recorder, &segment_id, &frame);
    let sample = if advanced {
        sample_fresh_core(recorder, record, &frame, at_ms, &segment_id)
    } else {
        None
    };
    let comparison = match &sample {
        _ if !advanced => TerminalWorkerComparison::BaselineInvalid,
        None | Some((SampleGate::Interval, _)) => TerminalWorkerComparison::Unsampled,
        Some((SampleGate::Grid | SampleGate::Budget, _)) => TerminalWorkerComparison::BudgetSkipped,
        Some((SampleGate::Sample, Some(_))) => TerminalWorkerComparison::Different,
        Some((SampleGate::Sample, None)) => TerminalWorkerComparison::Equal,
    };
    let difference = sample.and_then(|(_, difference)| difference);
    let emission_bytes = approximate_cell_frame_bytes(&frame);
    let emission = TerminalWorkerEmissionRecord {
        segment_id,
        emitted_at_ms: at_ms,
        stream: identity_of(&frame),
        full: frame.full,
        frame: Arc::clone(&frame),
        comparison,
        difference: difference.clone(),
    };
    let retained = retain_worker_record(
        &mut recorder.retention,
        &mut recorder.emissions,
        EMISSION_POOL,
        emission,
        emission_bytes,
        None,
    );
    if !retained {
        // One frame alone over the cell budget: the segment is unavailable
        // rather than retained unbounded, and emission is not held up for it.
        recorder.invalidate_fold(Reason::FrameOverBudget);
    }
    Some(EmissionConflict {
        stream_id: frame.stream_id.clone(),
        grid_epoch: frame.grid_epoch.clone(),
        seq: frame.seq.to_string(),
        difference: difference?,
    })
}

fn identity_of(frame: &CellGridFrame) -> TerminalCaptureStreamIdentity {
    TerminalCaptureStreamIdentity {
        stream_id: frame.stream_id.clone(),
        grid_epoch: frame.grid_epoch.clone(),
        seq: frame.seq.to_string(),
        base_seq: (!frame.full).then(|| frame.base_seq.to_string()),
        cols: frame.cols,
        rows: frame.rows,
    }
}

/// A full establishes the fold; a delta advances it through the production
/// fold. The fold is REPLACED, never mutated, so a frame this recorder already
/// retained as evidence is never rewritten by a later fold.
fn advance_fold(recorder: &mut WorkerRecorder, segment_id: &str, frame: &CellGridFrame) -> bool {
    if frame.full {
        let mut baseline = clone_cell_grid_frame(frame);
        normalize_cell_grid_frame(&mut baseline);
        if canonical_view_of_frame(&baseline).is_none() {
            recorder.invalidate_fold(Reason::BaselineInvalidated);
            return false;
        }
        recorder.fold = Some(Arc::new(baseline));
        recorder.fold_segment_id = Some(segment_id.to_owned());
        return true;
    }
    let Some(fold) = recorder
        .fold
        .as_ref()
        .filter(|_| recorder.fold_segment_id.as_deref() == Some(segment_id))
    else {
        return false;
    };
    let Some(batch) = fold_cell_delta_batch(fold, std::slice::from_ref(frame)) else {
        recorder.invalidate_fold(Reason::BaselineInvalidated);
        return false;
    };
    let mut folded = batch.frame;
    normalize_cell_grid_frame(&mut folded);
    recorder.fold = Some(Arc::new(folded));
    true
}

/// One fresh viewport-only core scan at this emission's generation and
/// sequence, never enumerating history (`tail_rows = Some(0)`).
fn sample_fresh_core(
    recorder: &mut WorkerRecorder,
    record: &SessionRecord,
    frame: &CellGridFrame,
    at_ms: u64,
    segment_id: &str,
) -> Option<(SampleGate, Option<TerminalCanonicalDifference>)> {
    let fold = Arc::clone(recorder.fold.as_ref()?);
    let before = Instant::now();
    let gate =
        recorder.classify_core_sample_gate(before, u64::from(frame.cols) * u64::from(frame.rows));
    if gate != SampleGate::Sample {
        return Some((gate, None));
    }
    let core = record.terminal_core.as_ref();
    let Ok(origin) = scrollback_origin(core, record.cell_emit.scrollback_origin) else {
        recorder.invalidate_fold(Reason::CoreExportUnavailable);
        return Some((SampleGate::Budget, None));
    };
    let epoch = record.cell_emit.grid_epoch();
    let core_frame = grid_to_cell_frame(core, frame.seq, &epoch, &frame.stream_id, Some(0), origin);
    let elapsed_us = u64::try_from(before.elapsed().as_micros()).unwrap_or(u64::MAX);
    recorder.note_core_sample_elapsed(before, elapsed_us, at_ms);
    let (Some(core_view), Some(fold_view)) = (
        canonical_view_of_frame(&core_frame),
        canonical_view_of_frame(&fold),
    ) else {
        recorder.invalidate_fold(Reason::BaselineInvalidated);
        return Some((SampleGate::Budget, None));
    };
    let difference = compare_canonical_views(&fold_view, &core_view);
    let bytes = approximate_cell_frame_bytes(&core_frame) + approximate_cell_frame_bytes(&fold);
    let sample = TerminalWorkerCoreSampleRecord {
        segment_id: segment_id.to_owned(),
        sampled_at_ms: at_ms,
        stream: TerminalCaptureStreamIdentity {
            base_seq: None,
            cols: core_frame.cols,
            rows: core_frame.rows,
            ..identity_of(frame)
        },
        elapsed_us,
        core_frame: Arc::new(core_frame),
        // The fold is replaced, never mutated, so retaining it here freezes
        // exactly the state that was compared.
        fold_frame: fold,
        comparison: if difference.is_some() {
            TerminalWorkerComparison::Different
        } else {
            TerminalWorkerComparison::Equal
        },
        difference: difference.clone(),
    };
    retain_worker_record(
        &mut recorder.retention,
        &mut recorder.core_samples,
        CORE_SAMPLE_POOL,
        sample,
        bytes,
        None,
    );
    Some((gate, difference))
}
