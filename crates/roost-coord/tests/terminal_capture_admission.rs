//! Terminal capture admission: frozen coordinator evidence on the wire, the
//! nested-section envelope check, idempotent retries, one capture at a time
//! with a manual cooldown, unarmed one-shots, and the worker-failure mapping
//! that must come back as a result the browser can retry.
//!
//! Ports the "terminal capture admission" cases of
//! `apps/coord/tests/terminal/capture/terminal-capture-bridge.test.ts`; its
//! lease cases are `terminal_capture_lease.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod terminal_capture_support;

use connectrpc::{ConnectError, ErrorCode};
use roost_coord::terminal_capture::worker_call::request_terminal_capture;
use roost_proto::TerminalCaptureAction;
use roost_protocol::terminal_capture::TerminalCaptureErrorCode as Failure;
use roost_protocol::terminal_capture::TerminalCaptureStatus as Status;
use roost_protocol::terminal_capture::bundle::{
    TERMINAL_INCIDENT_SCHEMA, TerminalCaptureLayer, TerminalCaptureReason,
};
use roost_protocol::terminal_capture::command::{
    TerminalCaptureActionName, TerminalCaptureCommand, TerminalCaptureResult,
};
use roost_protocol::terminal_capture::envelope::{EvidenceOwner, check_terminal_capture_envelope};
use roost_protocol::wire::WorkerFp;
use serde_json::{Value, json};
use terminal_capture_support::{
    CAPTURE_1, CAPTURE_2, CAPTURE_3, CAPTURE_WORKER, CaptureFixture, CaptureReply, RECORDING_A,
    RECORDING_B, RECORDING_C, SESSION_A, SESSION_B, WORKER_CAPTURE_PATH, admitted,
    browser_evidence, canonical_frame, capture_worker_ack, request, without_layer_section,
};

async fn capture_as(
    f: &CaptureFixture,
    session_id: &str,
    recording_id: &str,
    capture_id: &str,
    evidence: &str,
) -> Result<TerminalCaptureResult, ConnectError> {
    let request = request(
        TerminalCaptureAction::Capture,
        session_id,
        recording_id,
        capture_id,
        evidence,
    );
    f.handle(&request, &f.device_a).await
}

async fn capture(
    f: &CaptureFixture,
    capture_id: &str,
) -> Result<TerminalCaptureResult, ConnectError> {
    capture_as(f, SESSION_A, RECORDING_A, capture_id, "").await
}

async fn start(f: &CaptureFixture) {
    let request = request(
        TerminalCaptureAction::Start,
        SESSION_A,
        RECORDING_A,
        CAPTURE_1,
        "",
    );
    f.handle(&request, &f.device_a).await.unwrap();
}

fn assert_refused(
    outcome: Result<TerminalCaptureResult, ConnectError>,
    code: ErrorCode,
    message: &str,
) {
    let error = outcome.expect_err("the capture is refused");
    assert_eq!(
        (error.code, error.message.as_deref()),
        (code, Some(message))
    );
}

fn command() -> TerminalCaptureCommand {
    TerminalCaptureCommand {
        action: TerminalCaptureActionName::Capture,
        session_id: SESSION_A.to_owned(),
        recording_id: RECORDING_A.to_owned(),
        capture_id: CAPTURE_1.to_owned(),
        reason: TerminalCaptureReason::Manual,
        browser_evidence_json: String::new(),
    }
}

#[tokio::test]
async fn carries_frozen_coordinator_evidence_and_answers_the_workers_file() {
    let f = CaptureFixture::new("admit-evidence").await;
    start(&f).await;
    let recorder = &f.services.terminal_capture.recorder;
    recorder.record(
        SESSION_A,
        &canonical_frame(3, 1, 1, None),
        admitted(true, 3, 0),
        0,
        1,
    );

    let result = capture_as(
        &f,
        SESSION_A,
        RECORDING_A,
        CAPTURE_1,
        &browser_evidence(CAPTURE_1, RECORDING_A, SESSION_A).to_string(),
    )
    .await
    .unwrap();
    assert_eq!(
        (
            result.action,
            result.status,
            result.path.as_deref(),
            result.byte_length
        ),
        (
            TerminalCaptureActionName::Capture,
            Status::Captured,
            Some(WORKER_CAPTURE_PATH),
            Some(4_096)
        )
    );
    assert_eq!(
        (result.expires_at_ms, result.error),
        (Some(1_800_000), None)
    );
    let frame = f.worker().commands.last().unwrap().clone();
    assert_eq!(
        (frame["kind"].as_str(), frame["recording_id"].as_str()),
        (Some("diag-terminal-capture"), Some(RECORDING_A))
    );
    // Each layer crosses the wire as an envelope with its section NESTED under
    // the layer's own member; the worker unwraps that member.
    let coordinator_json = frame["coordinator_evidence_json"].as_str().unwrap();
    let forwarded: Value = serde_json::from_str(coordinator_json).unwrap();
    assert_eq!(forwarded["schema"], TERMINAL_INCIDENT_SCHEMA);
    assert_eq!(
        [
            &forwarded["layer"],
            &forwarded["capture_id"],
            &forwarded["recording_id"],
            &forwarded["session_id"]
        ],
        [
            &json!("coordinator"),
            &json!(CAPTURE_1),
            &json!(RECORDING_A),
            &json!(SESSION_A)
        ]
    );
    assert_eq!(
        forwarded["coordinator"]["records"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(forwarded["coordinator"]["captured_at_ms"].as_u64().unwrap() > 0);
    let owner = EvidenceOwner {
        capture_id: CAPTURE_1,
        recording_id: RECORDING_A,
        session_id: SESSION_A,
    };
    assert!(
        check_terminal_capture_envelope(
            coordinator_json,
            TerminalCaptureLayer::Coordinator,
            &owner
        )
        .is_ok()
    );
    // The browser's own payload reached the worker unchanged, and freezing did
    // not drain the ring the lease keeps recording into.
    let browser: Value =
        serde_json::from_str(frame["browser_evidence_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        (&browser["layer"], &browser["browser"]["layer"]),
        (&json!("browser"), &json!("browser"))
    );
    let stats = recorder.stats(SESSION_A).unwrap();
    assert_eq!(
        (stats.recording_id.as_str(), stats.records),
        (RECORDING_A, 1)
    );
}

#[tokio::test]
async fn browser_evidence_with_no_nested_section_never_reaches_the_worker() {
    let f = CaptureFixture::new("admit-malformed").await;
    start(&f).await;
    let flattened =
        without_layer_section(browser_evidence(CAPTURE_1, RECORDING_A, SESSION_A)).to_string();
    let refused = capture_as(&f, SESSION_A, RECORDING_A, CAPTURE_1, &flattened).await;
    assert_refused(
        refused,
        ErrorCode::InvalidArgument,
        "evidence_malformed: browser",
    );
    assert_eq!(f.actions(), ["start"]);
}

#[tokio::test]
async fn an_idempotent_retry_returns_the_original_result_and_dispatches_once() {
    let f = CaptureFixture::new("admit-idempotent").await;
    start(&f).await;
    let first = capture(&f, CAPTURE_1).await.unwrap();
    assert_eq!(capture(&f, CAPTURE_1).await.unwrap(), first);
    assert_eq!(f.actions(), ["start", "capture"]);
}

#[tokio::test]
async fn one_capture_at_a_time_per_session_then_a_manual_cooldown() {
    let f = CaptureFixture::new("admit-in-flight").await;
    start(&f).await;
    f.script(CaptureReply::Park);
    let (held, ()) = tokio::join!(capture(&f, CAPTURE_1), async {
        while f.worker().parked.is_empty() {
            tokio::task::yield_now().await;
        }
        let second = capture(&f, CAPTURE_2).await;
        assert_refused(second, ErrorCode::Aborted, "capture_in_flight: capture_id");
        let request_id = f.worker().parked[0].0.clone();
        assert!(f.release(&request_id, capture_worker_ack("capture")));
    });
    assert_eq!(held.unwrap().status, Status::Captured);

    let cooled = capture(&f, CAPTURE_3).await;
    assert_refused(
        cooled,
        ErrorCode::ResourceExhausted,
        "rate_limited: capture_id",
    );
}

#[tokio::test]
async fn an_unarmed_one_shot_captures_but_never_overrides_an_active_lease() {
    let f = CaptureFixture::new("admit-one-shot").await;
    let evidence = browser_evidence(CAPTURE_1, RECORDING_B, SESSION_B).to_string();
    let one_shot = capture_as(&f, SESSION_B, RECORDING_B, CAPTURE_1, &evidence)
        .await
        .unwrap();
    assert_eq!(
        (one_shot.status, one_shot.path.as_deref()),
        (Status::Captured, Some(WORKER_CAPTURE_PATH))
    );
    // No lease was allocated, nothing was armed, and the coordinator reports
    // its own layer absent rather than inventing rows.
    assert!(!f.armed(SESSION_B));
    assert_eq!(f.worker().commands[0]["coordinator_evidence_json"], "");

    let request = request(
        TerminalCaptureAction::Start,
        SESSION_A,
        RECORDING_A,
        CAPTURE_1,
        "",
    );
    f.handle(&request, &f.device_b).await.unwrap();
    let refused = capture_as(&f, SESSION_A, RECORDING_C, CAPTURE_2, "").await;
    assert_refused(
        refused,
        ErrorCode::AlreadyExists,
        "lease_conflict: recording_id",
    );
    let stats = f
        .services
        .terminal_capture
        .recorder
        .stats(SESSION_A)
        .unwrap();
    assert_eq!(stats.recording_id, RECORDING_A);
}

#[tokio::test]
async fn a_worker_reported_failure_is_a_result_not_a_throw() {
    let f = CaptureFixture::new("admit-worker-error").await;
    start(&f).await;
    f.script(CaptureReply::Ack(json!({
        "status": "error",
        "path": null,
        "byte_length": null,
        "error": "storage_failed",
        "expires_at_ms": null,
        "recent_worker_capture": {
            "capture_id": CAPTURE_3,
            "path": WORKER_CAPTURE_PATH,
            "byte_length": 128,
            "status": "captured",
        },
    })));
    let failed = capture(&f, CAPTURE_1).await.unwrap();
    assert_eq!(
        (failed.status, failed.error, failed.path.as_deref()),
        (Status::Error, Some(Failure::StorageFailed), None)
    );
    let recent = failed.recent_worker_capture.unwrap();
    assert_eq!(
        (recent.capture_id.as_str(), recent.byte_length),
        (CAPTURE_3, 128)
    );
    // The failure is not cached and the lease still records, so the browser can
    // retry the same capture ID with its frozen evidence.
    assert!(f.armed(SESSION_A));
}

#[tokio::test]
async fn an_unparseable_worker_acknowledgement_is_a_worker_failure() {
    let f = CaptureFixture::new("admit-unparseable").await;
    start(&f).await;
    f.script(CaptureReply::Ack(json!({
        "status": "captured",
        "path": WORKER_CAPTURE_PATH,
        "byte_length": -1,
        "error": null,
    })));
    let failed = capture(&f, CAPTURE_1).await.unwrap();
    assert_eq!(
        (failed.status, failed.error, failed.path.as_deref()),
        (Status::Error, Some(Failure::WorkerFailed), None)
    );
}

#[tokio::test]
async fn a_deadline_and_a_dropped_socket_map_to_their_own_fixed_codes() {
    let f = CaptureFixture::new("admit-deadline").await;
    f.script(CaptureReply::Park);
    let relay = &f.services.scrollback;
    let worker_fp = WorkerFp::try_from(CAPTURE_WORKER).unwrap();
    // The contract's deadline is ten seconds; the mapping is what this proves.
    let timed_out = request_terminal_capture(relay, &worker_fp, &command(), "", 25).await;
    assert_eq!(timed_out, Err(Failure::WorkerTimeout));
    f.handle.revoke();
    let offline = request_terminal_capture(relay, &worker_fp, &command(), "", 10_000).await;
    assert_eq!(offline, Err(Failure::WorkerOffline));
}
