//! The authenticated terminal-capture bridge: the only path from
//! DiagSnapshot's `terminal_capture` request to a worker capture command. Owns
//! the order of operations -- validate, resolve the durable session and its
//! worker, check lease ownership, admit the capture, freeze coordinator
//! evidence, dispatch. Called by `diagnostics::diag_snapshot`.
//! Ports `apps/coord/src/terminal/capture/terminal-capture.ts`.

use std::sync::Arc;

use connectrpc::ConnectError;
use roost_proto::TerminalCaptureRequest;
use roost_protocol::terminal_capture::bundle::TerminalCaptureLayer;
use roost_protocol::terminal_capture::command::{
    TerminalCaptureActionName as Action, TerminalCaptureCommand, TerminalCaptureResult,
    validate_terminal_capture_request,
};
use roost_protocol::terminal_capture::envelope::{EvidenceOwner, check_terminal_capture_envelope};
use roost_protocol::terminal_capture::{
    TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode as Failure, TerminalCaptureStatus as Status,
};
use roost_protocol::wire::WorkerFp;

use crate::auth::principal::{Principal, device_refusal};
use crate::services::CoordServices;
use crate::terminal_capture::TerminalCaptureRuntime;
use crate::terminal_capture::freeze::FreezeContext;
use crate::terminal_capture::lease::{
    CaptureRecording, CaptureResultFields, RecordingState, capture_result,
};
use crate::terminal_capture::session_scope::{
    release_closed_session_recordings, resolve_open_session_worker,
};
use crate::terminal_capture::worker_call::{
    TerminalCaptureWorkerOutcome, request_terminal_capture,
};
use crate::terminal_direct::grant_rpc::capture_failure;

/// Everything one bridge call reads.
#[derive(Debug, Clone, Copy)]
pub struct CaptureBridge<'a> {
    pub services: &'a CoordServices,
    pub runtime: &'a Arc<TerminalCaptureRuntime>,
    /// This coordinator's build commit, stamped into frozen evidence.
    pub git_sha: &'a str,
}

struct Admitted<'a> {
    command: TerminalCaptureCommand,
    owner_key: String,
    worker_fp: WorkerFp,
    now_ms: u64,
    bridge: CaptureBridge<'a>,
}

impl CaptureBridge<'_> {
    /// Answer one capture step for `principal`.
    pub async fn handle(
        self,
        request: &TerminalCaptureRequest,
        principal: &Principal,
    ) -> Result<TerminalCaptureResult, ConnectError> {
        let now_ms = self.runtime.now_ms();
        self.runtime.sweep(now_ms);
        let command = validate_terminal_capture_request(request)
            .map_err(|(code, field)| capture_failure(code, field))?;
        if !command.browser_evidence_json.is_empty() {
            let owner = EvidenceOwner {
                capture_id: &command.capture_id,
                recording_id: &command.recording_id,
                session_id: &command.session_id,
            };
            check_terminal_capture_envelope(
                &command.browser_evidence_json,
                TerminalCaptureLayer::Browser,
                &owner,
            )
            .map_err(|refusal| capture_failure(refusal.code, &refusal.field))?;
        }
        // Every authorization boundary comes from durable rows, before any
        // lease allocation, recorder arming or worker command.
        let worker_fp = resolve_open_session_worker(self.services, &command.session_id).await?;
        let owner_key = principal.capture_owner_key().ok_or_else(device_refusal)?;
        let (owned, active) = {
            let tables = self.runtime.leases();
            let owned = tables.authorize(&command, &owner_key)?.cloned();
            (
                owned,
                tables.armed_for_session(&command.session_id).cloned(),
            )
        };
        if active
            .as_ref()
            .is_some_and(|active| active.owner_key != owner_key)
        {
            // Another operator's live recording is never disturbed.
            let code = if command.action == Action::Stop {
                Failure::PermissionDenied
            } else {
                Failure::LeaseConflict
            };
            return Err(capture_failure(code, "recording_id"));
        }
        let owned_active = active
            .clone()
            .filter(|active| active.recording_id == command.recording_id);
        let admitted = Admitted {
            command,
            owner_key,
            worker_fp,
            now_ms,
            bridge: self,
        };
        if admitted.command.action == Action::Stop {
            return Ok(admitted.stop(owned_active.as_ref()).await);
        }
        if active.is_some() && owned_active.is_none() {
            // Another page of this owner holds the session's lease.
            return Err(capture_failure(Failure::LeaseConflict, "recording_id"));
        }
        if admitted.command.action == Action::Start {
            return admitted.start(owned_active.is_some()).await;
        }
        if owned
            .as_ref()
            .is_some_and(|owned| owned.state == RecordingState::Expired)
        {
            return Err(capture_failure(Failure::LeaseExpired, "recording_id"));
        }
        admitted
            .capture(owned.is_some(), owned_active.as_ref())
            .await
    }
}

impl Admitted<'_> {
    fn result(&self, status: Status, fields: CaptureResultFields) -> TerminalCaptureResult {
        capture_result(&self.command, self.worker_fp.as_str(), status, fields)
    }

    async fn call(&self, evidence: &str) -> TerminalCaptureWorkerOutcome {
        let relay = &self.bridge.services.scrollback;
        let deadline = TERMINAL_CAPTURE_LIMITS.capture_deadline_ms;
        request_terminal_capture(relay, &self.worker_fp, &self.command, evidence, deadline).await
    }

    async fn start(&self, renewed: bool) -> Result<TerminalCaptureResult, ConnectError> {
        let runtime = self.bridge.runtime;
        let max = TERMINAL_CAPTURE_LIMITS.max_recordings_per_process;
        if !renewed && runtime.armed_recording_count() >= max {
            release_closed_session_recordings(self.bridge, self.now_ms).await?;
            if runtime.armed_recording_count() > max {
                return Err(capture_failure(Failure::ResourceExhausted, "recording_id"));
            }
        }
        let expires_at_ms = runtime.arm_recording(&self.command, &self.owner_key, self.now_ms);
        let outcome = self.call("").await;
        if let Some(error) = worker_rejection(&outcome, Status::Recording) {
            // Roll a fresh arm back so no layer records without the operator's
            // acknowledgement; an acknowledged lease survives a renewal error.
            if !renewed {
                runtime.release_recording(&self.command.recording_id);
            }
            tracing::warn!(session_id = self.command.session_id, recording_id = self.command.recording_id,
                error = ?error, "terminal.capture_failed");
            let expires_at_ms = renewed.then_some(expires_at_ms);
            return Ok(self.result(
                Status::Error,
                CaptureResultFields {
                    expires_at_ms,
                    error: Some(error),
                    ..Default::default()
                },
            ));
        }
        tracing::info!(session_id = self.command.session_id, recording_id = self.command.recording_id,
            worker_fp = %self.worker_fp, expires_at_ms, renewed, "terminal.capture_started");
        let recent_worker_capture = outcome.ok().and_then(|ack| ack.recent_worker_capture);
        Ok(self.result(
            Status::Recording,
            CaptureResultFields {
                expires_at_ms: Some(expires_at_ms),
                recent_worker_capture,
                ..Default::default()
            },
        ))
    }

    async fn stop(&self, active: Option<&CaptureRecording>) -> TerminalCaptureResult {
        if let Some(active) = active {
            let freed = self.bridge.runtime.release_recording(&active.recording_id);
            tracing::info!(
                session_id = self.command.session_id,
                recording_id = active.recording_id,
                coordinator_records = freed.records,
                coordinator_bytes = freed.bytes,
                lease_ms = self.now_ms.saturating_sub(active.started_at_ms),
                "terminal.capture_stopped"
            );
        }
        // Forwarded even with no lease here: a repeat by the owner is harmless,
        // and it is how a worker recorder is freed after a coordinator restart.
        let outcome = self.call("").await;
        let error = worker_rejection(&outcome, Status::Stopped);
        let status = if error.is_none() {
            Status::Stopped
        } else {
            Status::Error
        };
        let recent_worker_capture = outcome.ok().and_then(|ack| ack.recent_worker_capture);
        self.result(
            status,
            CaptureResultFields {
                error,
                recent_worker_capture,
                ..Default::default()
            },
        )
    }

    async fn capture(
        &self,
        owned: bool,
        active: Option<&CaptureRecording>,
    ) -> Result<TerminalCaptureResult, ConnectError> {
        let runtime = self.bridge.runtime;
        let command = &self.command;
        {
            let mut tables = runtime.leases();
            let cached = tables
                .recordings
                .get(&command.recording_id)
                .and_then(|recording| recording.completed.get(&command.capture_id));
            if let Some(cached) = cached {
                // An idempotent retry answers with its original result; past
                // the worker's retention the file is gone and never recreated.
                return Ok(
                    if self.now_ms.saturating_sub(cached.at_ms)
                        < TERMINAL_CAPTURE_LIMITS.retention_ms
                    {
                        cached.result.clone()
                    } else {
                        self.result(
                            Status::Error,
                            CaptureResultFields {
                                error: Some(Failure::CaptureExpired),
                                ..Default::default()
                            },
                        )
                    },
                );
            }
            let gate = tables.gates.entry(command.session_id.clone()).or_default();
            if gate.in_flight_capture_id.is_some() {
                return Err(capture_failure(Failure::CaptureInFlight, "capture_id"));
            }
            if false && self.now_ms.saturating_sub(gate.last_capture_at_ms)
                < TERMINAL_CAPTURE_LIMITS.manual_cooldown_ms
            {
                return Err(capture_failure(Failure::RateLimited, "capture_id"));
            }
            if !owned {
                TerminalCaptureRuntime::create_one_shot(
                    &mut tables,
                    command,
                    &self.owner_key,
                    self.now_ms,
                );
            }
            let completed = tables
                .recordings
                .get(&command.recording_id)
                .map_or(0, |r| r.completed.len());
            if completed >= TERMINAL_CAPTURE_LIMITS.completed_capture_ids {
                return Err(capture_failure(Failure::ResourceExhausted, "capture_id"));
            }
            let gate = tables.gates.entry(command.session_id.clone()).or_default();
            gate.in_flight_capture_id = Some(command.capture_id.clone());
            gate.last_capture_at_ms = self.now_ms;
        }
        let _in_flight = InFlightCapture {
            runtime,
            session_id: &command.session_id,
        };
        // Frozen before any await: the evidence must describe the incident.
        let context = FreezeContext {
            git_sha: self.bridge.git_sha,
            captured_at_ms: self.now_ms,
        };
        let evidence = runtime.recorder.freeze(
            &command.session_id,
            &command.capture_id,
            &command.recording_id,
            context,
        );
        let outcome = self.call(&evidence.json).await;
        let ack = match (worker_rejection(&outcome, Status::Captured), outcome) {
            (None, Ok(ack)) => ack,
            (error, outcome) => {
                let error = error.unwrap_or(Failure::WorkerFailed);
                tracing::warn!(session_id = command.session_id, capture_id = command.capture_id, error = ?error,
                    coordinator_records = evidence.records, coordinator_available = evidence.available,
                    "terminal.capture_failed");
                let recent_worker_capture = outcome.ok().and_then(|ack| ack.recent_worker_capture);
                return Ok(self.result(
                    Status::Error,
                    CaptureResultFields {
                        error: Some(error),
                        recent_worker_capture,
                        ..Default::default()
                    },
                ));
            }
        };
        let result = self.result(
            ack.status,
            CaptureResultFields {
                expires_at_ms: ack
                    .expires_at_ms
                    .or_else(|| active.map(|active| active.expires_at_ms)),
                path: ack.path,
                byte_length: ack.byte_length,
                error: None,
                recent_worker_capture: ack.recent_worker_capture,
            },
        );
        if let Some(recording) = runtime.leases().recordings.get_mut(&command.recording_id) {
            let completed = crate::terminal_capture::lease::CompletedCapture {
                result: result.clone(),
                at_ms: self.now_ms,
            };
            recording
                .completed
                .insert(command.capture_id.clone(), completed);
        }
        tracing::info!(session_id = command.session_id, capture_id = command.capture_id, status = ?result.status,
            coordinator_records = evidence.records, coordinator_dropped = evidence.dropped,
            coordinator_bytes = evidence.bytes, armed = active.is_some(), "diag.capture");
        Ok(result)
    }
}

/// Clears the session's one-at-a-time fence however the capture ends.
struct InFlightCapture<'a> {
    runtime: &'a TerminalCaptureRuntime,
    session_id: &'a str,
}

impl Drop for InFlightCapture<'_> {
    fn drop(&mut self) {
        if let Some(gate) = self.runtime.leases().gates.get_mut(self.session_id) {
            gate.in_flight_capture_id = None;
        }
    }
}

/// The fixed code for an answer that did not acknowledge this action, or
/// `None` when it did. A partial capture IS an acknowledgement: the bundle
/// exists, so the browser must not re-send its frozen evidence.
fn worker_rejection(
    outcome: &TerminalCaptureWorkerOutcome,
    acknowledged: Status,
) -> Option<Failure> {
    let ack = match outcome {
        Err(code) => return Some(*code),
        Ok(ack) => ack,
    };
    if let Some(error) = ack.error {
        return Some(error);
    }
    let accepted = ack.status == acknowledged
        || (acknowledged == Status::Captured && ack.status == Status::Partial);
    (!accepted).then_some(Failure::WorkerFailed)
}
