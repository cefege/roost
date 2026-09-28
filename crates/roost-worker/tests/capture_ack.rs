//! `recent_worker_capture` on every worker capture ack: a requested capture
//! reports its own file and never echoes itself as a worker-detected incident;
//! only a worker-local emission conflict populates it, and a peer-authored
//! trigger cannot buy that claim. Ports `apps/worker/tests/terminal/
//! terminal-capture-ack.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod capture_support;
mod terminal_stream_support;

use capture_support::{
    CaptureHarness, RECORDING_ID, browser_payload, command, mismatched_full_frame, read_bundle,
};
use roost_protocol::terminal_capture::TerminalCaptureStatus;
use roost_worker::browser_commands::diagnostics::{
    CaptureAction, CaptureCommand, DiagnosticReports,
};
use serde_json::json;
use terminal_stream_support::{COLS, ROWS, STREAM_A};

const MANUAL_CAPTURE_ID: &str = "eeeeeeee-0000-4000-8000-00000000ea01";
const SECOND_CAPTURE_ID: &str = "eeeeeeee-0000-4000-8000-00000000ea02";
const LEASE_CAPTURE_ID: &str = "eeeeeeee-0000-4000-8000-00000000ea03";
const STOP_CAPTURE_ID: &str = "eeeeeeee-0000-4000-8000-00000000ea04";
const SPOOF_CAPTURE_ID: &str = "eeeeeeee-0000-4000-8000-00000000ea05";

async fn armed(label: &str) -> CaptureHarness {
    let harness = CaptureHarness::new(label);
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    harness.paint(&["FOOTER-12s", "row-1", "row-2"]);
    let start = harness.recorder.start_recording(command(
        CaptureAction::Start,
        RECORDING_ID,
        LEASE_CAPTURE_ID,
    ));
    assert_eq!(start.status, TerminalCaptureStatus::Recording);
    harness
}

fn capture(capture_id: &str) -> CaptureCommand {
    command(CaptureAction::Capture, RECORDING_ID, capture_id)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_requested_capture_reports_its_own_file_and_never_itself_as_an_incident() {
    let harness = armed("ack-own").await;
    let ack = harness.capture(capture(MANUAL_CAPTURE_ID)).await;
    assert_eq!(ack.error, None);
    assert!(ack.byte_length.unwrap() > 0);
    assert_eq!(ack.recent_worker_capture, None);

    // The retry replays the same result, so the false attribution cannot
    // reappear through the idempotency cache either.
    let retry = harness.capture(capture(MANUAL_CAPTURE_ID)).await;
    assert_eq!(
        (retry.path.clone(), retry.recent_worker_capture),
        (ack.path, None)
    );
    assert_eq!(harness.captured_files().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_local_emission_conflict_surfaces_on_the_next_acks() {
    let harness = armed("ack-local").await;
    let requested = harness.capture(capture(MANUAL_CAPTURE_ID)).await;
    assert_eq!(requested.recent_worker_capture, None);

    let tap = harness.recorder.tap();
    harness.with_record(|record| {
        tap.accepted_emission(record, Some(mismatched_full_frame(record, 1)))
    });
    harness.recorder.settle_scheduled_captures().await;
    let worker_file = harness
        .captured_files()
        .into_iter()
        .find(|name| !name.contains(MANUAL_CAPTURE_ID))
        .unwrap();

    // The browser never asked for that one: the lease ack is how it learns.
    let renewed = harness.recorder.start_recording(command(
        CaptureAction::Start,
        RECORDING_ID,
        LEASE_CAPTURE_ID,
    ));
    assert_eq!(renewed.status, TerminalCaptureStatus::Recording);
    let incident = renewed.recent_worker_capture.unwrap();
    assert_ne!(incident.capture_id, MANUAL_CAPTURE_ID);
    assert!(
        incident.path.ends_with(&worker_file),
        "{} / {worker_file}",
        incident.path
    );
    assert_eq!(
        read_bundle(&incident.path).await["trigger"]["reason"],
        json!("worker_emission")
    );

    // A later requested capture still reports its OWN file separately.
    let mut second = capture(SECOND_CAPTURE_ID);
    second.reason = "pre_repair".to_owned();
    let second = harness.capture(second).await;
    assert_ne!(second.path.as_deref(), Some(incident.path.as_str()));
    assert_eq!(second.recent_worker_capture.as_ref(), Some(&incident));

    let stopped = harness.recorder.stop_recording(command(
        CaptureAction::Stop,
        RECORDING_ID,
        STOP_CAPTURE_ID,
    ));
    assert_eq!(
        (stopped.status, stopped.recent_worker_capture),
        (TerminalCaptureStatus::Stopped, Some(incident))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_authored_worker_trigger_cannot_claim_a_worker_detection() {
    let harness = armed("ack-spoof").await;
    let mut payload = browser_payload(SPOOF_CAPTURE_ID, &[], &[]);
    payload["trigger"]["reason"] = json!("worker_emission");
    payload["trigger"]["origin"] = json!("worker");
    payload["trigger"]["detail"] = json!("core_fold_disagreement");
    let mut spoofed = capture(SPOOF_CAPTURE_ID);
    spoofed.browser_evidence_json = payload.to_string();

    let ack = harness.capture(spoofed).await;
    assert_eq!((ack.error, ack.recent_worker_capture.clone()), (None, None));
    // The authoring layer is a structural fact: the bundle records the browser.
    assert_eq!(
        read_bundle(ack.path.as_deref().unwrap()).await["trigger"]["origin"],
        json!("browser")
    );
    let renewed = harness.recorder.start_recording(command(
        CaptureAction::Start,
        RECORDING_ID,
        LEASE_CAPTURE_ID,
    ));
    assert_eq!(renewed.recent_worker_capture, None);
}
