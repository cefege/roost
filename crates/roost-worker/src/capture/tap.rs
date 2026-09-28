//! The guarded taps the terminal data path calls — retained bytes, accepted and
//! rejected emissions, resize boundaries — over the state they share with the
//! lease façade. Ports the tap half of `apps/worker/src/diag/terminal-capture.ts`
//! (`noteRetainedRawChunk`, `noteAcceptedCellEmission`, `noteRejectedCellEmission`,
//! `noteResizeInstall`, `noteResizeResult`); `session::emit` holds a
//! [`CaptureTap`]. Each tap settles on one atomic load while nothing is armed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine as _;
use roost_protocol::cell::CellGridFrame;
use roost_protocol::terminal_capture::bundle::{
    TerminalCoverageReason, TerminalWorkerRawRecord, TerminalWorkerResizeOutcome,
    TerminalWorkerResizeRecord,
};
use roost_protocol::terminal_capture::view::TerminalCanonicalDifference;
use roost_protocol::viewport::TerminalGeometry;
use tokio::runtime::Handle;
use tokio::sync::watch;

use super::byte_window::ByteWindows;
use super::emission::record_accepted_emission;
use super::finish::schedule_worker_local_capture;
use super::now_ms;
use super::pools::{RAW_POOL, raw_record_bytes, retain_resize_record, retain_worker_record};
use super::registry::Registry;
use super::storage::CaptureStorage;
use super::worker_section::WorkerProcessIdentity;
use crate::session::types::SessionRecord;

/// The state every tap and the façade share. Lock order: a session record
/// (held by the caller) → `registry` → `windows`; nothing here takes another.
#[derive(Debug)]
pub struct CaptureShared {
    pub registry: Mutex<Registry>,
    pub windows: Mutex<ByteWindows>,
    /// v2 `_armedCount`: read on every retained chunk, so the unarmed path is
    /// one load rather than a lock.
    pub armed: AtomicUsize,
    pub storage: Arc<CaptureStorage>,
    pub process: WorkerProcessIdentity,
    pub runtime: Handle,
    /// Worker-local captures whose write has not settled.
    pub scheduled: watch::Sender<usize>,
}

impl CaptureShared {
    pub fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn windows(&self) -> MutexGuard<'_, ByteWindows> {
        self.windows.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn any_armed(&self) -> bool {
        self.armed.load(Ordering::Acquire) > 0
    }

    /// Re-read the armed count after a registry change, under its lock.
    pub fn note_armed(&self, registry: &Registry) {
        self.armed.store(registry.armed_count(), Ordering::Release);
    }
}

/// A sequenced resize boundary as the capture records it (v2
/// `LiveResizeCapture`'s identity half).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeBoundaryNote {
    pub resize_seq: u64,
    /// `head_seq` when the capture gate installed.
    pub install_seq: u64,
    pub from: (u16, u16),
    pub to: (u16, u16),
}

/// The data path's handle on the recorder. The default is detached: an
/// emitter built without a recorder (a test's) records nothing.
#[derive(Debug, Clone, Default)]
pub struct CaptureTap {
    shared: Option<Arc<CaptureShared>>,
}

impl CaptureTap {
    pub(super) fn attached(shared: Arc<CaptureShared>) -> Self {
        Self {
            shared: Some(shared),
        }
    }

    /// The shared state, when a recording is armed anywhere on this worker.
    fn armed(&self) -> Option<&Arc<CaptureShared>> {
        self.shared.as_ref().filter(|shared| shared.any_armed())
    }

    /// Whether an accepted frame is worth copying for the recorder.
    pub fn wants_emissions(&self) -> bool {
        self.armed().is_some()
    }

    /// One retained PTY chunk with its exact absolute bounds, AFTER the ring
    /// took it: the always-on window always, the armed recorder's raw chain
    /// with its exact offsets when one is armed.
    pub fn retain_output(&self, record: &SessionRecord, end_seq: u64, chunk: &[u8]) {
        let Some(shared) = &self.shared else {
            return;
        };
        let session_id = record.session_id().as_str();
        shared.windows().push(session_id, chunk, end_seq);
        if !shared.any_armed() {
            return;
        }
        let mut registry = shared.registry();
        let Some(armed) = registry.active(session_id) else {
            return;
        };
        let recorder = &mut armed.recorder;
        let Some(segment_id) = recorder
            .open_segment()
            .map(|segment| segment.segment_id.clone())
        else {
            return;
        };
        let start = end_seq.saturating_sub(chunk.len() as u64);
        let raw = TerminalWorkerRawRecord {
            segment_id,
            at_ms: now_ms(),
            start_offset: start.to_string(),
            end_offset: end_seq.to_string(),
            base64: base64::engine::general_purpose::STANDARD.encode(chunk),
        };
        let bytes = raw_record_bytes(chunk.len());
        retain_worker_record(
            &mut recorder.retention,
            &mut recorder.raw,
            RAW_POOL,
            raw,
            bytes,
            Some((start, end_seq)),
        );
    }

    /// One ACCEPTED cell emission. Latches and schedules at most one
    /// worker-local capture per (recording, stream, epoch, reason), under the
    /// session-wide automatic floor.
    pub fn accepted_emission(&self, record: &SessionRecord, frame: Option<CellGridFrame>) {
        let (Some(shared), Some(frame)) = (self.armed(), frame) else {
            return;
        };
        let session_id = record.session_id().as_str();
        let mut registry = shared.registry();
        let Some(armed) = registry.active(session_id) else {
            return;
        };
        let frame = Arc::new(frame);
        let Some(conflict) =
            record_accepted_emission(&mut armed.recorder, record, Arc::clone(&frame))
        else {
            return;
        };
        let now = now_ms();
        let recorder = &mut armed.recorder;
        let latch_key = format!(
            "{}|{}|{}|worker_emission",
            recorder.recording_id, conflict.stream_id, conflict.grid_epoch
        );
        if !recorder.latch_automatic_capture(&latch_key, now) {
            return;
        }
        let (row, column) = match conflict.difference {
            TerminalCanonicalDifference::Row { row, column, .. } => (Some(row), Some(column)),
            TerminalCanonicalDifference::State { .. } => (None, None),
        };
        tracing::warn!(
            session_id,
            stream_id = %conflict.stream_id,
            grid_epoch = %conflict.grid_epoch,
            seq = %conflict.seq,
            difference = ?conflict.difference,
            ?row,
            ?column,
            "terminal.emission_conflict: the shipped fold and a fresh core scan disagree"
        );
        schedule_worker_local_capture(
            shared,
            armed,
            record,
            &frame,
            &conflict.seq,
            &latch_key,
            now,
        );
    }

    /// A dropped or rejected frame: the fold can no longer reproduce the
    /// shipped screen, so it is invalid until the next accepted full.
    pub fn rejected_emission(&self, record: &SessionRecord, reason: TerminalCoverageReason) {
        let Some(shared) = self.armed() else {
            return;
        };
        if let Some(armed) = shared.registry().active(record.session_id().as_str()) {
            armed.recorder.invalidate_fold(reason);
        }
    }

    /// A sequenced resize gate opened; `resize_result` completes the record.
    pub fn resize_install(&self, record: &SessionRecord, note: &ResizeBoundaryNote) {
        let Some(shared) = self.armed() else {
            return;
        };
        let mut registry = shared.registry();
        let Some(armed) = registry.active(record.session_id().as_str()) else {
            return;
        };
        let recorder = &mut armed.recorder;
        let Some(segment_id) = recorder
            .open_segment()
            .map(|segment| segment.segment_id.clone())
        else {
            return;
        };
        let resize = TerminalWorkerResizeRecord {
            segment_id,
            at_ms: now_ms(),
            resize_seq: note.resize_seq,
            install_offset: note.install_seq.to_string(),
            boundary_offset: None,
            from: geometry(note.from),
            to: geometry(note.to),
            outcome: TerminalWorkerResizeOutcome::Unknown,
            grid_epoch_before: record.cell_emit.grid_epoch(),
            grid_epoch_after: None,
            captured_bytes: 0,
        };
        retain_resize_record(&mut recorder.retention, &mut recorder.resizes, resize);
    }

    /// The keeper-acknowledged parse boundary, or its absence. `boundary_seq`
    /// is the raw offset the answer landed at — never a request time, which
    /// proves nothing about where the new geometry applied.
    pub fn resize_result(
        &self,
        record: &SessionRecord,
        note: &ResizeBoundaryNote,
        outcome: TerminalWorkerResizeOutcome,
        captured_bytes: u64,
        boundary_seq: Option<u64>,
    ) {
        let Some(shared) = self.armed() else {
            return;
        };
        let mut registry = shared.registry();
        let Some(armed) = registry.active(record.session_id().as_str()) else {
            return;
        };
        let install = note.install_seq.to_string();
        let matching = armed.recorder.resizes.iter_mut().rev().find(|resize| {
            resize.resize_seq == note.resize_seq && resize.install_offset == install
        });
        if let Some(resize) = matching {
            resize.outcome = outcome;
            resize.captured_bytes = captured_bytes;
            resize.grid_epoch_after = Some(record.cell_emit.grid_epoch());
            resize.boundary_offset = boundary_seq.map(|seq| seq.to_string());
        }
    }
}

fn geometry((cols, rows): (u16, u16)) -> TerminalGeometry {
    TerminalGeometry {
        cols: u32::from(cols),
        rows: u32::from(rows),
    }
}
