//! Terminal-capture lease registry: who owns a recording, when its server-time
//! lease expires, the per-session capture admission gate, and the bounded
//! completed-capture cache that makes one CAPTURE idempotent. Arms and disarms
//! the coordinator recorder so no lease transition can leave records behind.
//! Used only by `terminal_capture::bridge`.
//! Ports `apps/coord/src/terminal/capture/terminal-capture-lease.ts`, its
//! `captureResult` included
//! (`captureFailure` is `terminal_direct::grant_rpc::capture_failure`, and
//! `captureOwnerKey` is `Principal::capture_owner_key`).
//! The command and result types are `roost_protocol::terminal_capture::command`.

use std::collections::HashMap;
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::Duration;

use connectrpc::ConnectError;
use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;
use roost_protocol::terminal_capture::TerminalCaptureErrorCode as Failure;
use roost_protocol::terminal_capture::command::{TerminalCaptureCommand, TerminalCaptureResult};
use roost_protocol::terminal_capture::{TerminalCaptureFileRef, TerminalCaptureStatus};

use crate::terminal_capture::TerminalCaptureRuntime;
use crate::terminal_capture::recorder::ReleasedRecords;
use crate::terminal_direct::grant_rpc::capture_failure;

/// A lease's life stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
    /// Acknowledged and recording.
    Armed,
    /// Kept so a late CAPTURE is told the lease expired.
    Expired,
    /// An unarmed manual capture's idempotency record.
    OneShot,
}

/// One idempotent capture answer.
#[derive(Debug, Clone)]
pub(crate) struct CompletedCapture {
    pub(crate) result: TerminalCaptureResult,
    pub(crate) at_ms: u64,
}

/// One recording id, bound to ONE session and ONE owner for its whole life.
#[derive(Debug, Clone)]
pub struct CaptureRecording {
    pub recording_id: String,
    pub session_id: String,
    /// Derived from the authenticated principal, never from a request body.
    pub owner_key: String,
    pub state: RecordingState,
    pub started_at_ms: u64,
    pub expires_at_ms: u64,
    pub(crate) retain_until_ms: u64,
    pub(crate) completed: HashMap<String, CompletedCapture>,
}

/// One outstanding capture and one manual cooldown per SESSION.
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionCaptureGate {
    pub(crate) in_flight_capture_id: Option<String>,
    pub(crate) last_capture_at_ms: u64,
}

/// The registry's tables.
#[derive(Debug, Default)]
pub struct LeaseTables {
    pub(crate) recordings: HashMap<String, CaptureRecording>,
    pub(crate) armed_by_session: HashMap<String, String>,
    pub(crate) gates: HashMap<String, SessionCaptureGate>,
}

impl LeaseTables {
    /// The session's acknowledged lease.
    pub(crate) fn armed_for_session(&self, session_id: &str) -> Option<&CaptureRecording> {
        let recording_id = self.armed_by_session.get(session_id)?;
        self.recordings
            .get(recording_id)
            .filter(|recording| recording.state == RecordingState::Armed)
    }

    /// A recording id is bound to one session and one owner.
    pub(crate) fn authorize(
        &self,
        command: &TerminalCaptureCommand,
        owner_key: &str,
    ) -> Result<Option<&CaptureRecording>, ConnectError> {
        let Some(recording) = self.recordings.get(&command.recording_id) else {
            return Ok(None);
        };
        if recording.owner_key != owner_key {
            return Err(capture_failure(Failure::PermissionDenied, "recording_id"));
        }
        if recording.session_id != command.session_id {
            return Err(capture_failure(Failure::LeaseConflict, "session_id"));
        }
        Ok(Some(recording))
    }

    /// Retained records are bounded process-wide, oldest first; an armed
    /// lease is never evicted to make room.
    pub(crate) fn evict_retained(&mut self, headroom: usize) {
        let mut retained: Vec<(u64, String)> = self
            .recordings
            .values()
            .filter(|recording| recording.state != RecordingState::Armed)
            .map(|recording| (recording.started_at_ms, recording.recording_id.clone()))
            .collect();
        let excess = (retained.len() + headroom).saturating_sub(TERMINAL_CAPTURE_LIMITS.completed_capture_ids);
        retained.sort();
        for (_, recording_id) in retained.into_iter().take(excess) {
            self.recordings.remove(&recording_id);
        }
    }
}

impl TerminalCaptureRuntime {
    pub(crate) fn leases(&self) -> MutexGuard<'_, LeaseTables> {
        self.leases.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many sessions hold an acknowledged lease.
    #[must_use]
    pub fn armed_recording_count(&self) -> usize {
        self.leases().armed_by_session.len()
    }

    /// Create or renew one lease and arm the recorder with it. A renewal keeps
    /// the evidence the operator armed the lease to collect.
    pub(crate) fn arm_recording(
        self: &Arc<Self>,
        command: &TerminalCaptureCommand,
        owner_key: &str,
        now_ms: u64,
    ) -> u64 {
        let expires_at_ms = now_ms.saturating_add(TERMINAL_CAPTURE_LIMITS.lease_ms);
        {
            let mut tables = self.leases();
            let recording = tables
                .recordings
                .entry(command.recording_id.clone())
                .or_insert_with(|| CaptureRecording {
                    recording_id: command.recording_id.clone(),
                    session_id: command.session_id.clone(),
                    owner_key: owner_key.to_owned(),
                    state: RecordingState::Armed,
                    started_at_ms: now_ms,
                    expires_at_ms: now_ms,
                    retain_until_ms: now_ms,
                    completed: HashMap::new(),
                });
            recording.state = RecordingState::Armed;
            recording.expires_at_ms = expires_at_ms;
            recording.retain_until_ms = expires_at_ms;
            tables
                .armed_by_session
                .insert(command.session_id.clone(), command.recording_id.clone());
        }
        self.schedule_sweep(TERMINAL_CAPTURE_LIMITS.lease_ms);
        self.recorder.arm(&command.session_id, &command.recording_id);
        expires_at_ms
    }

    /// Sweep once the lease can have expired. A renewal's earlier sweep finds
    /// the lease still live and changes nothing.
    fn schedule_sweep(self: &Arc<Self>, delay_ms: u64) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("terminal capture: no runtime to schedule the lease sweep on; the next request sweeps");
            return;
        };
        let runtime = Arc::downgrade(self);
        let delay = Duration::from_millis(delay_ms.max(1).saturating_add(1_000));
        handle.spawn(async move {
            tokio::time::sleep(delay).await;
            if let Some(runtime) = runtime.upgrade() {
                runtime.sweep(runtime.now_ms());
            }
        });
    }

    /// An unarmed manual capture: no lease, no renewal, and its idempotent
    /// result retained with the other completed-capture records.
    pub(crate) fn create_one_shot(
        tables: &mut LeaseTables,
        command: &TerminalCaptureCommand,
        owner_key: &str,
        now_ms: u64,
    ) {
        tables.evict_retained(1);
        tables.recordings.insert(
            command.recording_id.clone(),
            CaptureRecording {
                recording_id: command.recording_id.clone(),
                session_id: command.session_id.clone(),
                owner_key: owner_key.to_owned(),
                state: RecordingState::OneShot,
                started_at_ms: now_ms,
                expires_at_ms: now_ms,
                retain_until_ms: now_ms.saturating_add(TERMINAL_CAPTURE_LIMITS.retention_ms),
                completed: HashMap::new(),
            },
        );
    }

    /// Drop the lease and free the coordinator records. Saved incident files
    /// belong to the worker and are never touched here.
    pub(crate) fn release_recording(&self, recording_id: &str) -> ReleasedRecords {
        let session_id = {
            let mut tables = self.leases();
            let Some(recording) = tables.recordings.remove(recording_id) else {
                return ReleasedRecords::default();
            };
            if tables.armed_by_session.get(&recording.session_id).map(String::as_str)
                == Some(recording_id)
            {
                tables.armed_by_session.remove(&recording.session_id);
            }
            // A stop racing a capture leaves that capture outstanding, so its
            // one-at-a-time fence survives and the sweep reaps it later.
            if tables
                .gates
                .get(&recording.session_id)
                .is_some_and(|gate| gate.in_flight_capture_id.is_none())
            {
                tables.gates.remove(&recording.session_id);
            }
            recording.session_id
        };
        self.recorder.disarm(&session_id)
    }

    /// Expire a lease but keep its id, so a late CAPTURE is told it expired
    /// instead of quietly becoming an unarmed one-shot.
    pub(crate) fn expire_recording(&self, recording_id: &str, now_ms: u64, reason: &str) {
        let Some(mut recording) = self.leases().recordings.get(recording_id).cloned() else {
            return;
        };
        let freed = self.release_recording(recording_id);
        recording.state = RecordingState::Expired;
        recording.retain_until_ms = now_ms.saturating_add(TERMINAL_CAPTURE_LIMITS.retention_ms);
        tracing::info!(session_id = recording.session_id, recording_id, reason,
            coordinator_records = freed.records, coordinator_bytes = freed.bytes,
            "terminal.capture_expired");
        self.leases().recordings.insert(recording_id.to_owned(), recording);
    }

    /// Server-time expiry sweep; tests drive it at an arbitrary instant.
    pub fn sweep(&self, now_ms: u64) {
        let (expired, stale): (Vec<String>, Vec<String>) = {
            let tables = self.leases();
            let expired = tables
                .recordings
                .values()
                .filter(|r| r.state == RecordingState::Armed && now_ms >= r.expires_at_ms)
                .map(|r| r.recording_id.clone())
                .collect();
            let stale = tables
                .recordings
                .values()
                .filter(|r| r.state != RecordingState::Armed && now_ms >= r.retain_until_ms)
                .map(|r| r.recording_id.clone())
                .collect();
            (expired, stale)
        };
        for recording_id in &expired {
            self.expire_recording(recording_id, now_ms, "lease_expired");
        }
        let mut tables = self.leases();
        for recording_id in &stale {
            tables.recordings.remove(recording_id);
        }
        let LeaseTables {
            gates,
            armed_by_session,
            ..
        } = &mut *tables;
        gates.retain(|session_id, gate| {
            gate.in_flight_capture_id.is_some()
                || armed_by_session.contains_key(session_id)
                || now_ms.saturating_sub(gate.last_capture_at_ms)
                    < TERMINAL_CAPTURE_LIMITS.manual_cooldown_ms
        });
        tables.evict_retained(0);
    }
}

/// The fields of a result that vary by outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureResultFields {
    pub expires_at_ms: Option<u64>,
    pub path: Option<String>,
    pub byte_length: Option<u64>,
    pub error: Option<Failure>,
    pub recent_worker_capture: Option<TerminalCaptureFileRef>,
}

/// v2 `captureResult`: the command's identity plus the outcome fields.
#[must_use]
pub fn capture_result(
    command: &TerminalCaptureCommand,
    worker_fp: &str,
    status: TerminalCaptureStatus,
    fields: CaptureResultFields,
) -> TerminalCaptureResult {
    TerminalCaptureResult {
        capture_id: command.capture_id.clone(),
        recording_id: command.recording_id.clone(),
        session_id: command.session_id.clone(),
        action: command.action,
        status,
        expires_at_ms: fields.expires_at_ms,
        worker_fp: Some(worker_fp.to_owned()),
        path: fields.path,
        byte_length: fields.byte_length,
        error: fields.error,
        recent_worker_capture: fields.recent_worker_capture,
    }
}
