//! Terminal capture leases: ownership, server-time expiry, the per-process
//! recording cap with closed-session reclaim, and the START rollback when the
//! worker refuses.
//!
//! Ports the "terminal capture lease" cases of
//! `apps/coord/tests/terminal/capture/terminal-capture-bridge.test.ts`; its
//! admission cases are `terminal_capture_admission.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod terminal_capture_support;

use connectrpc::{ConnectError, ErrorCode};
use roost_coord::auth::principal::Principal;
use roost_proto::TerminalCaptureAction;
use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;
use roost_protocol::terminal_capture::TerminalCaptureErrorCode;
use roost_protocol::terminal_capture::TerminalCaptureStatus as Status;
use roost_protocol::terminal_capture::command::{TerminalCaptureActionName, TerminalCaptureResult};
use terminal_capture_support::{
    CAPTURE_1, CAPTURE_WORKER, CaptureFixture, CaptureReply, RECLAIMED_SESSION, RECORDING_A,
    RECORDING_B, RECORDING_C, SESSION_A, SESSION_B, SESSION_C, UNKNOWN_SESSION, admitted,
    browser_evidence, canonical_frame, request,
};

async fn start(
    fixture: &CaptureFixture,
    session_id: &str,
    recording_id: &str,
    principal: &Principal,
) -> Result<TerminalCaptureResult, ConnectError> {
    let request = request(
        TerminalCaptureAction::Start,
        session_id,
        recording_id,
        CAPTURE_1,
        "",
    );
    fixture.handle(&request, principal).await
}

async fn stop(
    fixture: &CaptureFixture,
    recording_id: &str,
    principal: &Principal,
) -> Result<TerminalCaptureResult, ConnectError> {
    let request = request(
        TerminalCaptureAction::Stop,
        SESSION_A,
        recording_id,
        CAPTURE_1,
        "",
    );
    fixture.handle(&request, principal).await
}

fn assert_refused(
    outcome: Result<TerminalCaptureResult, ConnectError>,
    code: ErrorCode,
    message: Option<&str>,
) {
    let error = outcome.expect_err("the step is refused");
    assert_eq!(error.code, code, "{error:?}");
    if let Some(message) = message {
        assert_eq!(error.message.as_deref(), Some(message));
    }
}

fn record_one_frame(fixture: &CaptureFixture, seq: u64) {
    fixture.services.terminal_capture.recorder.record(
        SESSION_A,
        &canonical_frame(seq, 1, 1, None),
        admitted(true, seq, 0),
        0,
        1,
    );
}

#[tokio::test]
async fn arms_the_coordinator_recorder_and_renews_without_clearing_evidence() {
    let f = CaptureFixture::new("lease-renew").await;
    let armed = start(&f, SESSION_A, RECORDING_A, &f.device_a)
        .await
        .unwrap();
    assert_eq!(
        (
            armed.session_id.as_str(),
            armed.recording_id.as_str(),
            armed.action,
            armed.status
        ),
        (
            SESSION_A,
            RECORDING_A,
            TerminalCaptureActionName::Start,
            Status::Recording
        )
    );
    assert_eq!(armed.worker_fp.as_deref(), Some(CAPTURE_WORKER));
    assert_eq!((armed.path.as_deref(), armed.error), (None, None));
    let now = f.services.terminal_capture.now_ms();
    assert!(armed.expires_at_ms.unwrap() > now);
    assert!(f.armed(SESSION_A));
    record_one_frame(&f, 1);

    let renewed = start(&f, SESSION_A, RECORDING_A, &f.device_a)
        .await
        .unwrap();
    assert_eq!(renewed.status, Status::Recording);
    assert!(renewed.expires_at_ms.unwrap() >= armed.expires_at_ms.unwrap());
    let stats = f
        .services
        .terminal_capture
        .recorder
        .stats(SESSION_A)
        .unwrap();
    assert_eq!(
        (stats.recording_id.as_str(), stats.records),
        (RECORDING_A, 1)
    );
    assert_eq!(f.actions(), ["start", "start"]);
}

#[tokio::test]
async fn another_recording_or_another_device_cannot_take_an_armed_session() {
    let f = CaptureFixture::new("lease-conflict").await;
    start(&f, SESSION_A, RECORDING_A, &f.device_a)
        .await
        .unwrap();
    let conflict = start(&f, SESSION_A, RECORDING_B, &f.device_a).await;
    assert_refused(
        conflict,
        ErrorCode::AlreadyExists,
        Some("lease_conflict: recording_id"),
    );
    let foreign = start(&f, SESSION_A, RECORDING_B, &f.device_b).await;
    assert_refused(foreign, ErrorCode::AlreadyExists, None);
    let stolen = start(&f, SESSION_A, RECORDING_A, &f.device_b).await;
    assert_refused(
        stolen,
        ErrorCode::PermissionDenied,
        Some("permission_denied: recording_id"),
    );
    // The refused calls never reached the worker and never disturbed the lease.
    assert_eq!(f.actions(), ["start"]);
    let stats = f
        .services
        .terminal_capture
        .recorder
        .stats(SESSION_A)
        .unwrap();
    assert_eq!(stats.recording_id, RECORDING_A);
}

#[tokio::test]
async fn an_expired_lease_disarms_the_recorder_and_refuses_a_late_capture() {
    let f = CaptureFixture::new("lease-expired").await;
    start(&f, SESSION_A, RECORDING_A, &f.device_a)
        .await
        .unwrap();
    let runtime = &f.services.terminal_capture;
    runtime.sweep(runtime.now_ms() + TERMINAL_CAPTURE_LIMITS.lease_ms + 1);
    assert!(!f.armed(SESSION_A));

    let late = request(
        TerminalCaptureAction::Capture,
        SESSION_A,
        RECORDING_A,
        CAPTURE_1,
        &browser_evidence(CAPTURE_1, RECORDING_A, SESSION_A).to_string(),
    );
    let refused = f.handle(&late, &f.device_a).await;
    assert_refused(
        refused,
        ErrorCode::FailedPrecondition,
        Some("lease_expired: recording_id"),
    );
    assert_eq!(f.actions(), ["start"]);

    // Re-arming is an explicit START, never a silent renewal.
    let rearmed = start(&f, SESSION_A, RECORDING_A, &f.device_a)
        .await
        .unwrap();
    assert_eq!(rearmed.status, Status::Recording);
    assert!(f.armed(SESSION_A));
}

#[tokio::test]
async fn stop_is_owner_only_frees_coordinator_state_and_stays_idempotent() {
    let f = CaptureFixture::new("lease-stop").await;
    start(&f, SESSION_A, RECORDING_A, &f.device_a)
        .await
        .unwrap();
    let foreign = stop(&f, RECORDING_A, &f.device_b).await;
    assert_refused(
        foreign,
        ErrorCode::PermissionDenied,
        Some("permission_denied: recording_id"),
    );
    assert!(f.armed(SESSION_A));

    let stopped = stop(&f, RECORDING_A, &f.device_a).await.unwrap();
    assert_eq!(
        (stopped.action, stopped.status, stopped.expires_at_ms),
        (TerminalCaptureActionName::Stop, Status::Stopped, None)
    );
    assert!(!f.armed(SESSION_A));
    assert_eq!(
        stop(&f, RECORDING_A, &f.device_a).await.unwrap().status,
        Status::Stopped
    );
    assert_eq!(f.actions(), ["start", "stop", "stop"]);
}

#[tokio::test]
async fn a_third_recording_is_refused_without_evicting_either_live_one() {
    let f = CaptureFixture::new("lease-cap").await;
    start(&f, SESSION_A, RECORDING_A, &f.device_a)
        .await
        .unwrap();
    start(&f, SESSION_B, RECORDING_B, &f.device_a)
        .await
        .unwrap();
    let third = start(&f, SESSION_C, RECORDING_C, &f.device_a).await;
    assert_refused(
        third,
        ErrorCode::ResourceExhausted,
        Some("resource_exhausted: recording_id"),
    );
    assert!(f.armed(SESSION_A));
    assert!(f.armed(SESSION_B));
}

#[tokio::test]
async fn a_lease_on_a_closed_session_is_reclaimed_instead_of_parking_a_slot() {
    let f = CaptureFixture::new("lease-reclaim").await;
    start(&f, SESSION_A, RECORDING_A, &f.device_a)
        .await
        .unwrap();
    start(&f, RECLAIMED_SESSION, RECORDING_B, &f.device_a)
        .await
        .unwrap();
    f.exec(&format!(
        "UPDATE sessions SET status = 'closed' WHERE id = '{RECLAIMED_SESSION}'"
    ))
    .await;

    let third = start(&f, SESSION_C, RECORDING_C, &f.device_a)
        .await
        .unwrap();
    assert_eq!(third.status, Status::Recording);
    assert!(f.armed(SESSION_A));
    assert!(!f.armed(RECLAIMED_SESSION));
}

#[tokio::test]
async fn an_unreachable_session_never_reaches_a_lease_or_the_worker() {
    let f = CaptureFixture::new("lease-unknown").await;
    let unknown = start(&f, UNKNOWN_SESSION, RECORDING_A, &f.device_a).await;
    assert_refused(
        unknown,
        ErrorCode::NotFound,
        Some("session_unknown: session_id"),
    );
    assert!(f.worker().commands.is_empty());
    assert!(!f.armed(UNKNOWN_SESSION));
}

#[tokio::test]
async fn a_worker_that_refuses_start_rolls_the_fresh_arm_back() {
    let f = CaptureFixture::new("lease-rollback").await;
    f.script(CaptureReply::Drop);
    let refused = start(&f, SESSION_A, RECORDING_A, &f.device_a)
        .await
        .unwrap();
    assert_eq!(
        (refused.status, refused.error, refused.expires_at_ms),
        (
            Status::Error,
            Some(TerminalCaptureErrorCode::WorkerOffline),
            None
        )
    );
    assert!(!f.armed(SESSION_A));
}
