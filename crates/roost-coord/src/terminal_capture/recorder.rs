//! Bounded coordinator-side cell records for opt-in terminal incident capture.
//! Armed and released only by the lease table (`terminal_capture::lease`); fed
//! by one narrow hook at the screen hub's accepted full / folded delta boundary
//! (`terminal_screen::replica_admission`); frozen by `terminal_capture::freeze`
//! before the bridge dispatches CAPTURE to a worker. Every bound comes from
//! `roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS`.
//! Ports the retention half of `apps/coord/src/terminal/capture/terminal-capture-recorder.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_protocol::cell::CellGridFrame;
use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;
use roost_protocol::terminal_capture::bundle::TerminalCaptureStreamIdentity;
use roost_protocol::terminal_capture::coordinator::{
    CoordinatorRepair, CoordinatorSendState, CoordinatorSnapshotState, SequenceGap,
    TerminalCoordinatorRecord,
};
use sha2::{Digest, Sha256};

use crate::coord_core::ids::{draw, render_v4};

/// Just enough of the admitted wire frame to name what the hub accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmittedFrameIdentity {
    pub full: bool,
    pub seq: u64,
    pub base_seq: u64,
}

const RECORD_JSON_OVERHEAD: usize = 320;
const FRAME_JSON_OVERHEAD: usize = 256;
const ROW_JSON_OVERHEAD: usize = 24;
const SPAN_JSON_OVERHEAD: usize = 56;

/// One retained record and its byte cost.
#[derive(Debug, Clone)]
pub(crate) struct RecordedFrame {
    pub(crate) record: TerminalCoordinatorRecord,
    pub(crate) bytes: usize,
}

/// A dropped span of sequences, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SequenceRange {
    pub(crate) start: String,
    pub(crate) end: String,
}

/// One armed session's retained evidence.
#[derive(Debug)]
pub(crate) struct ArmedSession {
    pub(crate) recording_id: String,
    pub(crate) frames: Vec<RecordedFrame>,
    pub(crate) bytes: usize,
    pub(crate) dropped_records: u64,
    pub(crate) dropped_bytes: u64,
    pub(crate) dropped_range: Option<SequenceRange>,
    pub(crate) over_budget_records: u64,
    pub(crate) over_budget_rows: u64,
}

/// Retained record count and byte cost for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorRecorderStats {
    pub recording_id: String,
    pub records: usize,
    pub bytes: usize,
    pub dropped: u64,
}

/// What a disarm freed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReleasedRecords {
    pub records: usize,
    pub bytes: usize,
}

/// Every armed session's records. Owned by `TerminalCaptureRuntime`.
#[derive(Debug)]
pub struct CoordinatorRecorder {
    armed: Mutex<HashMap<String, ArmedSession>>,
    /// Distinguishes a coordinator restart in an otherwise continuous bundle.
    pub(crate) process_id: String,
}

impl Default for CoordinatorRecorder {
    fn default() -> Self {
        Self {
            armed: Mutex::default(),
            process_id: coordinator_process_id(),
        }
    }
}

/// Minted once per recorder, never derived from a request. Without the kernel
/// CSPRNG the id falls back to one derived from this process and instant,
/// which still distinguishes a restart, and says so.
fn coordinator_process_id() -> String {
    match draw::<16>() {
        Ok(bytes) => render_v4(bytes),
        Err(error) => {
            tracing::warn!(%error, "terminal capture: /dev/urandom unavailable; deriving the process id");
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            let digest = Sha256::digest(format!("{}:{nanos}", std::process::id()));
            let mut bytes = [0_u8; 16];
            bytes.copy_from_slice(&digest[..16]);
            render_v4(bytes)
        }
    }
}

impl CoordinatorRecorder {
    pub(crate) fn lock(&self) -> MutexGuard<'_, HashMap<String, ArmedSession>> {
        self.armed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hot-path guard for the hub hook.
    #[must_use]
    pub fn armed(&self, session_id: &str) -> bool {
        self.lock().contains_key(session_id)
    }

    /// Idempotent for the same recording: a renewal never clears evidence. A
    /// different recording belongs to a different page and starts empty.
    pub fn arm(&self, session_id: &str, recording_id: &str) {
        let mut armed = self.lock();
        if armed
            .get(session_id)
            .is_some_and(|session| session.recording_id == recording_id)
        {
            return;
        }
        armed.insert(
            session_id.to_owned(),
            ArmedSession {
                recording_id: recording_id.to_owned(),
                frames: Vec::new(),
                bytes: 0,
                dropped_records: 0,
                dropped_bytes: 0,
                dropped_range: None,
                over_budget_records: 0,
                over_budget_rows: 0,
            },
        );
    }

    /// Frees every retained record. Saved bundles are the worker's to retain.
    pub fn disarm(&self, session_id: &str) -> ReleasedRecords {
        self.lock()
            .remove(session_id)
            .map_or_else(ReleasedRecords::default, |session| ReleasedRecords {
                records: session.frames.len(),
                bytes: session.bytes,
            })
    }

    /// Screen-hub hook, once per ACCEPTED full or folded delta, with the
    /// canonical the hub just installed. Copies the frame only when armed.
    pub fn record(
        &self,
        session_id: &str,
        canonical: &CellGridFrame,
        admitted: AdmittedFrameIdentity,
        watcher_count: usize,
        at_ms: u64,
    ) {
        let mut armed = self.lock();
        let Some(session) = armed.get_mut(session_id) else {
            return;
        };
        let frame_bytes = estimate_frame_json_bytes(canonical);
        let over_budget = frame_bytes > TERMINAL_CAPTURE_LIMITS.coordinator_evidence_bytes;
        let gap = sequence_gap(session.frames.last().map(|frame| &frame.record), canonical, admitted);
        let record = TerminalCoordinatorRecord {
            at_ms,
            stream: TerminalCaptureStreamIdentity {
                stream_id: canonical.stream_id.clone(),
                grid_epoch: canonical.grid_epoch.clone(),
                seq: canonical.seq.to_string(),
                base_seq: (!admitted.full).then(|| admitted.base_seq.to_string()),
                cols: canonical.cols,
                rows: canonical.rows,
            },
            admitted_full: admitted.full,
            accepted: true,
            canonical: (!over_budget).then(|| Arc::new(canonical.clone())),
            snapshot_state: CoordinatorSnapshotState::Installed,
            send_state: if watcher_count == 0 {
                CoordinatorSendState::NotSent
            } else {
                CoordinatorSendState::Queued
            },
            repair: if gap.is_none() {
                CoordinatorRepair::None
            } else {
                CoordinatorRepair::RequestedFull
            },
            gap,
        };
        if over_budget {
            session.over_budget_records += 1;
            session.over_budget_rows += canonical.viewport_rows.len() as u64;
        }
        let bytes = RECORD_JSON_OVERHEAD + if over_budget { 0 } else { frame_bytes };
        session.frames.push(RecordedFrame { record, bytes });
        session.bytes += bytes;
        evict_retained_frames(session);
    }

    /// Retained record count and byte cost for one session.
    #[must_use]
    pub fn stats(&self, session_id: &str) -> Option<CoordinatorRecorderStats> {
        self.lock().get(session_id).map(|session| CoordinatorRecorderStats {
            recording_id: session.recording_id.clone(),
            records: session.frames.len(),
            bytes: session.bytes,
            dropped: session.dropped_records,
        })
    }

    /// The retained records themselves, oldest first.
    #[must_use]
    pub fn records(&self, session_id: &str) -> Vec<TerminalCoordinatorRecord> {
        self.lock().get(session_id).map_or_else(Vec::new, |session| {
            session.frames.iter().map(|frame| frame.record.clone()).collect()
        })
    }
}

/// A full that does not continue the retained fold means the coordinator lost
/// its baseline and asked for a fresh one. Deltas cannot gap: the hub folds a
/// delta only when its base equals its cached sequence. A new stream or epoch
/// restarts the numbering and is not a gap.
fn sequence_gap(
    previous: Option<&TerminalCoordinatorRecord>,
    canonical: &CellGridFrame,
    admitted: AdmittedFrameIdentity,
) -> Option<SequenceGap> {
    let previous = previous?;
    if previous.stream.stream_id != canonical.stream_id
        || previous.stream.grid_epoch != canonical.grid_epoch
    {
        return None;
    }
    let previous_seq = previous.stream.seq.parse::<u64>().ok()?;
    let base = if admitted.full { previous_seq } else { admitted.base_seq };
    if base == previous_seq && canonical.seq == previous_seq.wrapping_add(1) {
        return None;
    }
    Some(SequenceGap {
        from: previous.stream.seq.clone(),
        to: canonical.seq.to_string(),
    })
}

/// Evict whole records oldest-first to the entry and byte bounds, then keep
/// dropping while the head carries no canonical, so the oldest retained record
/// is always a complete checkpoint rather than an orphan delta.
fn evict_retained_frames(session: &mut ArmedSession) {
    while session.frames.len() > TERMINAL_CAPTURE_LIMITS.layer_entries
        || session.bytes > TERMINAL_CAPTURE_LIMITS.layer_bytes
    {
        if !drop_oldest_frame(session) {
            return;
        }
    }
    while session.frames.len() > 1 && session.frames[0].record.canonical.is_none() {
        drop_oldest_frame(session);
    }
}

fn drop_oldest_frame(session: &mut ArmedSession) -> bool {
    if session.frames.is_empty() {
        return false;
    }
    let oldest = session.frames.remove(0);
    session.bytes -= oldest.bytes;
    session.dropped_records += 1;
    session.dropped_bytes += oldest.bytes as u64;
    let seq = oldest.record.stream.seq;
    match &mut session.dropped_range {
        Some(range) => range.end = seq,
        None => {
            session.dropped_range = Some(SequenceRange {
                start: seq.clone(),
                end: seq,
            });
        }
    }
    true
}

/// Exact-enough JSON cost of one canonical viewport without serializing it:
/// the bound must hold on the admission path.
fn estimate_frame_json_bytes(frame: &CellGridFrame) -> usize {
    let mut bytes = FRAME_JSON_OVERHEAD;
    for row in &frame.viewport_rows {
        bytes += ROW_JSON_OVERHEAD;
        for span in row.spans.iter() {
            bytes += SPAN_JSON_OVERHEAD + span.text.len();
            if let Some(uri) = &span.link_uri {
                bytes += uri.len() + 12;
            }
            if let Some(key) = &span.link_key {
                bytes += key.len() + 12;
            }
        }
    }
    bytes
}
