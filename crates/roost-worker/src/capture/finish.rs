//! One frozen capture's write and answer, and the worker-local capture an
//! emission conflict schedules. Ports `finishCapture`, `captureStatusOf` and
//! `scheduleWorkerLocalCapture` of `apps/worker/src/diag/
//! terminal-capture-write.ts`; called by `super::write` and `super::tap`. The
//! freeze happens in the caller's turn; only the gzip and the write await, and
//! only this module decides a capture's status and records it in a ledger.

use std::sync::Arc;

use serde_json::{Map, Value, json};

use roost_protocol::cell::CellGridFrame;
use roost_protocol::terminal_capture::{
    TerminalCaptureFileRef, TerminalCaptureStatus, TerminalCaptureWorkerAck,
};

use super::ack::{failure_ack, recent_worker_capture_for};
use super::bundle_writer::{IncidentBundleInput, write_terminal_incident_bundle};
use super::now_ms;
use super::recorder_state::CaptureLedger;
use super::registry::{ArmedRecording, Registry};
use super::section_coverage::is_complete_coverage;
use super::tap::CaptureShared;
use super::worker_section::{
    FrozenWorkerSection, StreamFacts, WorkerSectionRequest, freeze_worker_section,
};
use crate::session::ids::mint_uuid;
use crate::session::types::SessionRecord;

/// Which ledger a capture answers into once its write settles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerOwner {
    Armed {
        recording_id: String,
        armed_at_ms: u64,
    },
    OneShot,
}

/// A frozen capture waiting for its write.
#[derive(Debug)]
pub struct FinishRequest {
    pub capture_id: String,
    pub recording_id: String,
    pub session_id: String,
    pub owner: LedgerOwner,
    pub frozen: FrozenWorkerSection,
    /// TRUE only for the emission-conflict path the worker itself triggered:
    /// it alone may advance `recent_worker_local`.
    pub worker_local: bool,
    pub trigger: Value,
    /// Worker-owned fallback when `trigger` came from a peer.
    pub worker_trigger: Value,
    pub coordinator: Option<Map<String, Value>>,
    pub browser: Option<Map<String, Value>>,
    /// The ledger's recent worker capture at the freeze, for an answer whose
    /// ledger was released while the write ran.
    pub ledger_recent: Option<TerminalCaptureFileRef>,
}

/// Freeze inside the emission turn and let only the compression and the write
/// run later. The remote layers are explicitly absent: a worker-local trigger
/// has no browser or coordinator snapshot.
pub fn schedule_worker_local_capture(
    shared: &Arc<CaptureShared>,
    armed: &mut ArmedRecording,
    record: &SessionRecord,
    frame: &CellGridFrame,
    seq: &str,
    latch_key: &str,
    now: u64,
) {
    let capture_id = match mint_uuid() {
        Ok(capture_id) => capture_id,
        Err(error) => {
            tracing::error!(
                session_id = %armed.recorder.session_id,
                %error,
                "a worker-local capture could not be named, so none was taken"
            );
            return;
        }
    };
    let stream = StreamFacts {
        stream_id: frame.stream_id.clone(),
        cols: frame.cols,
        rows: frame.rows,
    };
    let recorder = &mut armed.recorder;
    let frozen = freeze_worker_section(WorkerSectionRequest {
        session_id: &recorder.session_id,
        process: &shared.process,
        recorder: Some(&*recorder),
        record: Some(record),
        stream: Some(&stream),
        history_ranges: &[],
        windows: &shared.windows(),
        captured_at_ms: now,
    });
    recorder.capture_in_flight = true;
    let trigger = json!({
        "reason": "worker_emission",
        "origin": "worker",
        "at_ms": now,
        "stream_id": frame.stream_id,
        "grid_epoch": frozen.section.stream.as_ref().map(|stream| stream.grid_epoch.clone()),
        "seq": seq,
        "detail": "core_fold_disagreement",
        "occurrence_count": recorder.occurrences.get(latch_key).copied().unwrap_or(1),
    });
    let request = FinishRequest {
        capture_id,
        recording_id: recorder.recording_id.clone(),
        session_id: recorder.session_id.clone(),
        owner: LedgerOwner::Armed {
            recording_id: recorder.recording_id.clone(),
            armed_at_ms: recorder.armed_at_ms,
        },
        frozen,
        worker_local: true,
        trigger: trigger.clone(),
        worker_trigger: trigger,
        coordinator: None,
        browser: None,
        ledger_recent: armed.ledger.recent_worker_local.clone(),
    };
    shared.scheduled.send_modify(|count| *count += 1);
    let task = Arc::clone(shared);
    shared.runtime.spawn(async move {
        finish_capture(&task, request).await;
        // Self-removal keeps the count bounded: production never waits on it.
        task.scheduled
            .send_modify(|count| *count = count.saturating_sub(1));
    });
}

/// Write the bundle, then answer from — and record into — the capture's ledger.
pub async fn finish_capture(
    shared: &Arc<CaptureShared>,
    request: FinishRequest,
) -> TerminalCaptureWorkerAck {
    let input = IncidentBundleInput {
        capture_id: request.capture_id.clone(),
        recording_id: request.recording_id.clone(),
        session_id: request.session_id.clone(),
        written_at_ms: now_ms(),
        trigger: request.trigger.clone(),
        worker_trigger: request.worker_trigger.clone(),
        coverage: request.frozen.coverage.clone(),
        worker: request.frozen.section.clone(),
        coordinator: request.coordinator.clone(),
        browser: request.browser.clone(),
    };
    let written = write_terminal_incident_bundle(&shared.storage, input).await;
    let mut guard = shared.registry();
    let registry: &mut Registry = &mut guard;
    let mut orphan = CaptureLedger {
        recent_worker_local: request.ledger_recent.clone(),
        ..CaptureLedger::default()
    };
    let (expires, ledger) = match &request.owner {
        LedgerOwner::Armed {
            recording_id,
            armed_at_ms,
        } => match registry.recorders.get_mut(&request.session_id) {
            Some(armed)
                if armed.recorder.recording_id == *recording_id
                    && armed.recorder.armed_at_ms == *armed_at_ms =>
            {
                armed.recorder.capture_in_flight = false;
                (Some(armed.recorder.expires_at_ms), &mut armed.ledger)
            }
            _ => (None, &mut orphan),
        },
        LedgerOwner::OneShot => {
            match registry.one_shot.ledger_if_present_mut(&request.session_id) {
                Some(ledger) => (None, ledger),
                None => (None, &mut orphan),
            }
        }
    };
    let capture_id = request.capture_id.as_str();
    let written = match written {
        Ok(written) => written,
        Err(code) => {
            tracing::warn!(
                session_id = %request.session_id,
                capture_id,
                error = ?code,
                "terminal.capture_failed"
            );
            return failure_ack(
                code,
                recent_worker_capture_for(ledger.recent_worker_local.as_ref(), capture_id),
                expires,
            );
        }
    };
    let status = capture_status_of(&request, written.trimmed);
    let path = written.stored.path.display().to_string();
    let file = TerminalCaptureFileRef {
        capture_id: capture_id.to_owned(),
        path: path.clone(),
        byte_length: written.stored.byte_length,
        status,
    };
    if request.worker_local {
        ledger.recent_worker_local = Some(file);
    }
    let ack = TerminalCaptureWorkerAck {
        status,
        path: Some(path),
        byte_length: Some(written.stored.byte_length),
        error: None,
        expires_at_ms: expires,
        recent_worker_capture: recent_worker_capture_for(
            ledger.recent_worker_local.as_ref(),
            capture_id,
        ),
    };
    ledger.completed.insert(capture_id.to_owned(), ack.clone());
    let coverage = &request.frozen.coverage;
    let reason = request
        .trigger
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    tracing::info!(
        session_id = %request.session_id,
        capture_id,
        byte_len = written.stored.byte_length,
        reason,
        ?status,
        cell_replay = ?coverage.cell_replay,
        core_replay = ?coverage.core_replay,
        core_comparison = ?coverage.core_comparison,
        "terminal.capture_saved"
    );
    ack
}

/// `captured` claims every layer present and every replay complete; anything
/// else is `partial`, so a reader never treats it as a whole picture.
fn capture_status_of(request: &FinishRequest, trimmed: bool) -> TerminalCaptureStatus {
    if trimmed || request.coordinator.is_none() || request.browser.is_none() {
        return TerminalCaptureStatus::Partial;
    }
    if is_complete_coverage(&request.frozen.coverage) {
        TerminalCaptureStatus::Captured
    } else {
        TerminalCaptureStatus::Partial
    }
}
