//! Freezes one armed (or unarmed) session's worker evidence into the immutable
//! `TerminalWorkerSection` plus its coverage report. Ports `freezeWorkerSection`
//! of `apps/worker/src/diag/terminal-capture-worker-section.ts`; called by
//! `super::write` synchronously, under the record and registry locks, before
//! any await, so the state recorded is the state at the trigger rather than
//! after the repair. A display-cell snapshot is never a parser checkpoint.

use std::collections::HashSet;

use roost_protocol::terminal_capture::bundle::{
    TerminalCaptureCoverageReport, TerminalCaptureDropCounters, TerminalCaptureLayer,
    TerminalCaptureOmission, TerminalCaptureOmissionKind, TerminalCaptureProcessIdentity,
    TerminalCaptureStreamIdentity, TerminalCoverageReason, TerminalWorkerSamplingStats,
    TerminalWorkerSection,
};
use roost_protocol::viewport::TerminalGeometry;

use super::byte_window::ByteWindows;
use super::evidence::HistoryRequest;
use super::recorder_state::WorkerRecorder;
use super::section_coverage::coverage_of;
use super::section_grid::read_grid_evidence;
use crate::session::types::SessionRecord;

/// Who this worker process is, as every section it freezes records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerProcessIdentity {
    /// Minted once per worker process, so a bundle written after a restart is
    /// distinguishable from one written before it.
    pub process_id: String,
    pub git_sha: String,
    pub artifact_version: String,
    pub worker_fp: String,
}

/// The coordinator-owned stream generation a section names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFacts {
    pub stream_id: String,
    pub cols: u32,
    pub rows: u32,
}

/// Everything one freeze reads.
#[derive(Debug, Clone, Copy)]
pub struct WorkerSectionRequest<'a> {
    pub session_id: &'a str,
    pub process: &'a WorkerProcessIdentity,
    pub recorder: Option<&'a WorkerRecorder>,
    pub record: Option<&'a SessionRecord>,
    pub stream: Option<&'a StreamFacts>,
    /// Absolute history rows the browser evidence named, end exclusive.
    pub history_ranges: &'a [HistoryRequest],
    pub windows: &'a ByteWindows,
    pub captured_at_ms: u64,
}

/// One frozen section and what it can prove.
#[derive(Debug, Clone)]
pub struct FrozenWorkerSection {
    pub section: TerminalWorkerSection,
    pub coverage: TerminalCaptureCoverageReport,
}

pub fn freeze_worker_section(request: WorkerSectionRequest<'_>) -> FrozenWorkerSection {
    let recorder = request.recorder;
    let mut omissions = recorder
        .map(|recorder| recorder.retention.omissions())
        .unwrap_or_default();
    let grid = read_grid_evidence(request.record, request.history_ranges, &mut omissions);
    let segment_ids: HashSet<&str> = recorder
        .map(|recorder| {
            recorder
                .segments
                .iter()
                .map(|segment| segment.segment_id.as_str())
                .collect()
        })
        .unwrap_or_default();
    let (mut emissions, mut core_samples, mut resizes, mut raw) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut held = 0usize;
    if let Some(recorder) = recorder {
        let kept = |segment_id: &str| segment_ids.contains(segment_id);
        emissions.extend(
            recorder
                .emissions
                .iter()
                .filter(|entry| kept(&entry.record.segment_id))
                .map(|entry| entry.record.clone()),
        );
        core_samples.extend(
            recorder
                .core_samples
                .iter()
                .filter(|entry| kept(&entry.record.segment_id))
                .map(|entry| entry.record.clone()),
        );
        resizes.extend(
            recorder
                .resizes
                .iter()
                .filter(|entry| kept(&entry.segment_id))
                .cloned(),
        );
        raw.extend(
            recorder
                .raw
                .iter()
                .filter(|entry| kept(&entry.record.segment_id))
                .map(|entry| entry.record.clone()),
        );
        held = recorder.emissions.len()
            + recorder.core_samples.len()
            + recorder.raw.len()
            + recorder.resizes.len();
    }
    // A record orphaned by segment eviction is unorderable, so it is dropped
    // and named rather than shipped as replayable evidence.
    let orphaned = held - (emissions.len() + core_samples.len() + raw.len() + resizes.len());
    if orphaned > 0 {
        omissions.push(TerminalCaptureOmission {
            kind: TerminalCaptureOmissionKind::Records,
            name: "worker.segment_orphans".to_owned(),
            reason: TerminalCoverageReason::SegmentEvicted,
            dropped_count: orphaned as u64,
            dropped_bytes: 0,
            range: None,
        });
    }
    let byte_capture = if raw.is_empty() {
        request.windows.snapshot(request.session_id)
    } else {
        None
    };
    let section = TerminalWorkerSection {
        layer: TerminalCaptureLayer::Worker,
        captured_at_ms: request.captured_at_ms,
        process: process_identity(request.process),
        stream: stream_identity_of(request.record, request.stream),
        geometry: grid.readable.then_some(TerminalGeometry {
            cols: grid.cols,
            rows: grid.rows,
        }),
        dropped: recorder.map_or(TerminalCaptureDropCounters::default(), |recorder| {
            recorder.retention.dropped
        }),
        omissions,
        segments: recorder
            .map(|recorder| recorder.segments.iter().cloned().collect())
            .unwrap_or_default(),
        emissions,
        core_samples,
        sampling: recorder.map_or(TerminalWorkerSamplingStats::default(), |recorder| {
            recorder.sampling
        }),
        resizes,
        raw,
        byte_capture,
        core_scrollback_tail: grid.tail,
        history_rows: grid.history_rows,
        history_ranges: grid.ranges,
        scrollback_total: grid.total,
        scrollback_origin: grid.origin.to_string(),
    };
    let coverage = coverage_of(recorder, &section);
    FrozenWorkerSection { section, coverage }
}

fn process_identity(process: &WorkerProcessIdentity) -> TerminalCaptureProcessIdentity {
    TerminalCaptureProcessIdentity {
        layer: TerminalCaptureLayer::Worker,
        process_id: process.process_id.clone(),
        git_sha: process.git_sha.clone(),
        artifact_version: process.artifact_version.clone(),
        // v2 stamped its pinned WASM core's digest here. This worker's core is
        // native (`roost_term`), so there is no WASM artifact to name.
        wasm_identity: None,
        worker_fp: Some(process.worker_fp.clone()),
        viewer_id: None,
        user_agent: None,
    }
}

fn stream_identity_of(
    record: Option<&SessionRecord>,
    stream: Option<&StreamFacts>,
) -> Option<TerminalCaptureStreamIdentity> {
    let (record, stream) = (record?, stream?);
    let core = record.terminal_core.as_ref();
    Some(TerminalCaptureStreamIdentity {
        stream_id: stream.stream_id.clone(),
        grid_epoch: record.cell_emit.grid_epoch(),
        seq: record.cell_emit.seq.to_string(),
        base_seq: None,
        cols: if stream.cols > 0 {
            stream.cols
        } else {
            u32::from(core.cols())
        },
        rows: if stream.rows > 0 {
            stream.rows
        } else {
            u32::from(core.rows())
        },
    })
}
