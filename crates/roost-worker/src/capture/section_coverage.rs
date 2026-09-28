//! What a frozen worker section can and cannot prove: the three coverage axes
//! and their machine-readable reasons. Ports `coverageOf` and its three
//! verdicts from `apps/worker/src/diag/terminal-capture-worker-section.ts`;
//! called by `super::worker_section`. A claim of `complete` needs proven
//! continuity; anything less is `partial` or `unavailable` with a reason.

use roost_protocol::terminal_capture::bundle::{
    TerminalCaptureCoverage as Coverage, TerminalCaptureCoverageReport,
    TerminalCoverageReason as Reason, TerminalWorkerSection,
};

use super::recorder_state::WorkerRecorder;

type Verdict = (Coverage, Vec<Reason>);

/// The coverage report for one frozen section.
pub fn coverage_of(
    recorder: Option<&WorkerRecorder>,
    section: &TerminalWorkerSection,
) -> TerminalCaptureCoverageReport {
    let (cell_replay, cell_replay_reasons) = cell_replay_coverage(recorder, section);
    let (core_replay, core_replay_reasons) = core_replay_coverage(recorder, section);
    let (core_comparison, core_comparison_reasons) = core_comparison_coverage(recorder);
    TerminalCaptureCoverageReport {
        cell_replay,
        cell_replay_reasons,
        core_replay,
        core_replay_reasons,
        core_comparison,
        core_comparison_reasons,
    }
}

fn unavailable() -> Verdict {
    (Coverage::Unavailable, vec![Reason::LayerUnavailable])
}

fn verdict_of(reasons: Vec<Reason>) -> Verdict {
    if reasons.is_empty() {
        (Coverage::Complete, vec![Reason::Complete])
    } else {
        (Coverage::Partial, reasons)
    }
}

fn cell_replay_coverage(
    recorder: Option<&WorkerRecorder>,
    section: &TerminalWorkerSection,
) -> Verdict {
    let Some(recorder) = recorder else {
        return unavailable();
    };
    if section.emissions.is_empty() {
        return unavailable();
    }
    let mut reasons = recorder.fold_reasons.clone();
    if recorder.fold.is_none() && reasons.is_empty() {
        reasons.push(Reason::BaselineInvalidated);
    }
    if recorder.retention.dropped.records > 0 && !reasons.contains(&Reason::SegmentEvicted) {
        reasons.push(Reason::SegmentEvicted);
    }
    verdict_of(reasons)
}

/// Complete ONLY with proven continuous output and geometry from this exact
/// core's initialization: a tail without the prefix that produced the current
/// parser state cannot attribute anything to the core.
fn core_replay_coverage(
    recorder: Option<&WorkerRecorder>,
    section: &TerminalWorkerSection,
) -> Verdict {
    let Some(recorder) = recorder else {
        return unavailable();
    };
    let mut reasons = Vec::new();
    if recorder.armed_offset > 0 || section.raw.is_empty() {
        reasons.push(Reason::MissingInitialPrefix);
    }
    if !recorder.retention.raw_prefix_complete {
        reasons.push(Reason::RawPrefixEvicted);
    }
    if section
        .resizes
        .iter()
        .any(|resize| resize.boundary_offset.is_none())
    {
        reasons.push(Reason::MissingResizeBoundary);
    }
    verdict_of(reasons)
}

/// Sampled equality proves only its own checkpoints, so the unsampled interval
/// between two scans is missing coverage rather than agreement.
fn core_comparison_coverage(recorder: Option<&WorkerRecorder>) -> Verdict {
    let Some(recorder) = recorder else {
        return unavailable();
    };
    let sampling = &recorder.sampling;
    let mut reasons = Vec::new();
    if sampling.skipped_grid > 0 {
        reasons.push(Reason::GridBudgetExceeded);
    }
    if sampling.skipped_budget > 0 {
        reasons.push(Reason::SampleBudgetExceeded);
    }
    if sampling.skipped_interval > 0 {
        reasons.push(Reason::CoreExportUnavailable);
    }
    if sampling.sampled == 0 {
        if reasons.is_empty() {
            reasons.push(Reason::LayerUnavailable);
        }
        return (Coverage::Unavailable, reasons);
    }
    verdict_of(reasons)
}

/// `captured` claims every layer present and every replay complete.
pub fn is_complete_coverage(coverage: &TerminalCaptureCoverageReport) -> bool {
    coverage.cell_replay == Coverage::Complete
        && coverage.core_replay == Coverage::Complete
        && coverage.core_comparison == Coverage::Complete
}
