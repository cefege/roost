//! Mutable state of one armed recording: its lease, segment chain, the
//! emitted-frame fold, the core-sampling gates and the automatic-capture latch.
//! Ports `apps/worker/src/diag/terminal-capture-recorder.ts`; owned by
//! `super::registry` and mutated by `super::recorder` and `super::emission`.
//! Bounded retention lives in `super::pools`; every bound comes from
//! `TERMINAL_CAPTURE_LIMITS`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use roost_protocol::cell::CellGridFrame;
use roost_protocol::terminal_capture::bundle::{
    TerminalCoverageReason, TerminalWorkerCoreSampleRecord, TerminalWorkerEmissionRecord,
    TerminalWorkerRawRecord, TerminalWorkerResizeRecord, TerminalWorkerSamplingStats,
    TerminalWorkerSegment, TerminalWorkerSegmentOpenReason as OpenReason,
};
use roost_protocol::terminal_capture::{
    TERMINAL_CAPTURE_LIMITS, TerminalCaptureFileRef, TerminalCaptureWorkerAck,
};
use roost_protocol::viewport::TerminalGeometry;

use super::pools::{Retained, RetentionLedger, SEGMENT_POOL, SEGMENT_RECORD_BYTES};
use crate::session::ids::{MintError, mint_uuid};

/// Bookkeeping that outlives one capture: completed results an RPC retry
/// replays, the last WORKER-TRIGGERED file, and the manual-capture floor.
/// `recent_worker_local` is only written by the emission-conflict path, so it
/// can never report a requested capture as a worker-detected incident.
#[derive(Debug, Clone, Default)]
pub struct CaptureLedger {
    pub completed: HashMap<String, TerminalCaptureWorkerAck>,
    pub recent_worker_local: Option<TerminalCaptureFileRef>,
    pub last_manual_ms: Option<u64>,
}

/// What arms a recorder.
#[derive(Debug, Clone)]
pub struct RecorderArming {
    pub session_id: String,
    pub worker_fp: String,
    pub recording_id: String,
    pub expires_at_ms: u64,
    pub at_ms: u64,
    pub head_seq: u64,
}

/// The identity a segment covers. v2 also keyed on the stream's version, which
/// moves only together with its stream id (a same-id request reuses the
/// generation), so the two keys open exactly the same segments.
#[derive(Debug, Clone)]
pub struct SegmentRequest<'a> {
    pub stream_id: &'a str,
    pub grid_epoch: &'a str,
    pub grid_epoch_base: &'a str,
    pub geometry: TerminalGeometry,
    pub head_seq: u64,
    pub at_ms: u64,
}

/// Which gate a fresh core scan passes or trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleGate {
    Sample,
    Grid,
    Budget,
    Interval,
}

/// One armed recording.
#[derive(Debug)]
pub struct WorkerRecorder {
    pub session_id: String,
    pub worker_fp: String,
    pub recording_id: String,
    pub expires_at_ms: u64,
    pub armed_at_ms: u64,
    /// Raw offset the recording started at. Non-zero means this core already
    /// parsed bytes nobody retained, so exact parser replay is impossible.
    pub armed_offset: u64,
    pub core_incarnation: u64,
    /// The chain; the newest entry is the open segment.
    pub segments: VecDeque<TerminalWorkerSegment>,
    pub segment_key: String,
    pub last_epoch_base: String,
    pub raw: VecDeque<Retained<TerminalWorkerRawRecord>>,
    pub emissions: VecDeque<Retained<TerminalWorkerEmissionRecord>>,
    pub core_samples: VecDeque<Retained<TerminalWorkerCoreSampleRecord>>,
    pub resizes: VecDeque<TerminalWorkerResizeRecord>,
    pub retention: RetentionLedger,
    pub sampling: TerminalWorkerSamplingStats,
    /// Viewport-only fold of the accepted frames; `None` until the next
    /// accepted full re-establishes a baseline.
    pub fold: Option<Arc<CellGridFrame>>,
    pub fold_segment_id: Option<String>,
    /// In first-seen order, as v2's `Set` iterates.
    pub fold_reasons: Vec<TerminalCoverageReason>,
    pub last_sample_mono: Option<Instant>,
    pub sample_suppressed_until_mono: Option<Instant>,
    /// One automatic capture per (recording, stream, epoch, reason).
    pub latches: Vec<String>,
    pub occurrences: HashMap<String, u64>,
    pub last_automatic_ms: Option<u64>,
    pub capture_in_flight: bool,
}

impl WorkerRecorder {
    pub fn new(arming: RecorderArming) -> Self {
        Self {
            session_id: arming.session_id,
            worker_fp: arming.worker_fp,
            recording_id: arming.recording_id,
            expires_at_ms: arming.expires_at_ms,
            armed_at_ms: arming.at_ms,
            armed_offset: arming.head_seq,
            core_incarnation: 1,
            segments: VecDeque::new(),
            segment_key: String::new(),
            last_epoch_base: String::new(),
            raw: VecDeque::new(),
            emissions: VecDeque::new(),
            core_samples: VecDeque::new(),
            resizes: VecDeque::new(),
            retention: RetentionLedger {
                raw_prefix_complete: arming.head_seq == 0,
                ..RetentionLedger::default()
            },
            sampling: TerminalWorkerSamplingStats::default(),
            fold: None,
            fold_segment_id: None,
            fold_reasons: Vec::new(),
            last_sample_mono: None,
            sample_suppressed_until_mono: None,
            latches: Vec::new(),
            occurrences: HashMap::new(),
            last_automatic_ms: None,
            capture_in_flight: false,
        }
    }

    /// The segment raw bytes and resizes are filed under.
    pub fn open_segment(&self) -> Option<&TerminalWorkerSegment> {
        self.segments.back()
    }

    /// The segment covering (core incarnation × stream × epoch). A transition
    /// opens a NEW segment and leaves the preceding one in the chain until
    /// ordinary eviction, so a resize cannot erase the evidence of its defect.
    /// Returns the open segment's id.
    pub fn open_worker_segment(
        &mut self,
        request: &SegmentRequest<'_>,
    ) -> Result<String, MintError> {
        let key = format!("{}\u{0}{}", request.stream_id, request.grid_epoch);
        if let Some(open) = self.segments.back().filter(|_| self.segment_key == key) {
            return Ok(open.segment_id.clone());
        }
        let segment_id = mint_uuid()?;
        let reason = match self.segments.back_mut() {
            None => OpenReason::Armed,
            Some(open) => {
                open.closed_at_ms = Some(request.at_ms);
                if self.last_epoch_base != request.grid_epoch_base {
                    OpenReason::CoreRebuild
                } else if open.stream_id != request.stream_id {
                    OpenReason::StreamChange
                } else {
                    OpenReason::EpochChange
                }
            }
        };
        if reason == OpenReason::CoreRebuild {
            self.core_incarnation += 1;
        }
        let transition = reason != OpenReason::Armed;
        self.segments.push_back(TerminalWorkerSegment {
            segment_id: segment_id.clone(),
            stream_id: request.stream_id.to_owned(),
            grid_epoch: request.grid_epoch.to_owned(),
            core_incarnation: self.core_incarnation,
            opened_at_ms: request.at_ms,
            closed_at_ms: None,
            open_reason: reason,
            geometry: request.geometry,
            open_offset: request.head_seq.to_string(),
        });
        self.retention.metadata_bytes += SEGMENT_RECORD_BYTES;
        while self.segments.len() > TERMINAL_CAPTURE_LIMITS.layer_entries && self.segments.len() > 1
        {
            self.segments.pop_front();
            self.retention.metadata_bytes -= SEGMENT_RECORD_BYTES;
            self.retention.dropped.records += 1;
            self.retention
                .note_omission(SEGMENT_POOL, SEGMENT_RECORD_BYTES, None);
        }
        self.segment_key = key;
        self.last_epoch_base = request.grid_epoch_base.to_owned();
        tracing::debug!(
            session_id = %self.session_id,
            %segment_id,
            ?reason,
            "a terminal capture segment opened"
        );
        // A new generation cannot inherit the previous fold: its absolute rows
        // and sequence space are a different grid.
        if transition {
            self.invalidate_fold(TerminalCoverageReason::BaselineInvalidated);
        }
        Ok(segment_id)
    }

    /// A dropped or rejected delta, a stream change and an epoch change all
    /// leave the fold unable to reproduce the shipped screen.
    pub fn invalidate_fold(&mut self, reason: TerminalCoverageReason) {
        self.fold = None;
        self.fold_segment_id = None;
        if !self.fold_reasons.contains(&reason) {
            self.fold_reasons.push(reason);
        }
    }

    /// A grid too large is never sampled; a scan that overran its budget
    /// suppresses the next second of scans.
    pub fn classify_core_sample_gate(&mut self, now: Instant, cells: u64) -> SampleGate {
        let limits = TERMINAL_CAPTURE_LIMITS;
        if cells > limits.core_sample_max_cells as u64 {
            self.sampling.skipped_grid += 1;
            return SampleGate::Grid;
        }
        if self
            .sample_suppressed_until_mono
            .is_some_and(|until| now < until)
        {
            self.sampling.skipped_budget += 1;
            return SampleGate::Budget;
        }
        let interval = Duration::from_millis(limits.core_sample_interval_ms);
        if self
            .last_sample_mono
            .is_some_and(|last| now.saturating_duration_since(last) < interval)
        {
            self.sampling.skipped_interval += 1;
            return SampleGate::Interval;
        }
        SampleGate::Sample
    }

    pub fn note_core_sample_elapsed(&mut self, now: Instant, elapsed_us: u64, at_ms: u64) {
        let limits = TERMINAL_CAPTURE_LIMITS;
        self.sampling.sampled += 1;
        self.last_sample_mono = Some(now);
        self.sampling.max_elapsed_us = self.sampling.max_elapsed_us.max(elapsed_us);
        if elapsed_us <= limits.core_sample_budget_us {
            return;
        }
        self.sample_suppressed_until_mono =
            Some(now + Duration::from_millis(limits.core_sample_suppress_ms));
        self.sampling.suppressed_until_ms = Some(at_ms + limits.core_sample_suppress_ms);
    }

    /// The per-identity latch collapses a repeating mismatch into ONE capture;
    /// the session-wide floor then bounds captures across identities, so a
    /// fresh epoch cannot buy a new capture.
    pub fn latch_automatic_capture(&mut self, key: &str, now_ms: u64) -> bool {
        *self.occurrences.entry(key.to_owned()).or_insert(0) += 1;
        if self.latches.iter().any(|held| held == key) {
            return false;
        }
        let cooldown = TERMINAL_CAPTURE_LIMITS.automatic_cooldown_ms;
        if self
            .last_automatic_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < cooldown)
        {
            return false;
        }
        self.latches.push(key.to_owned());
        self.last_automatic_ms = Some(now_ms);
        true
    }
}
