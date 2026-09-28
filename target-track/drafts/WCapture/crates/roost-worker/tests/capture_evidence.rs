//! Remote-evidence nesting for one CAPTURE: nested browser and coordinator
//! payloads land as their own sections with the browser's authored trigger; a
//! nested-but-invalid section is dropped WITHOUT costing the worker its own
//! evidence; a FLATTENED payload is refused outright. Ports `apps/worker/tests/
//! terminal/terminal-capture-evidence.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod capture_support;
mod terminal_stream_support;

use capture_support::{
    AT_MS, CaptureHarness, browser_payload, capture_command, coordinator_payload,
    flattened_browser_payload, invalid_trigger_payload, malformed_section_payload, read_bundle,
};
use roost_protocol::terminal_capture::{TerminalCaptureErrorCode, TerminalCaptureStatus};
use roost_worker::browser_commands::diagnostics::DiagnosticReports;
use serde_json::{Value, json};
use terminal_stream_support::{COLS, ROWS, STREAM_A};

async fn armed(label: &str) -> CaptureHarness {
    let harness = CaptureHarness::new(label);
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    assert_eq!(harness.start().status, TerminalCaptureStatus::Recording);
    harness
}

fn remote_omissions(bundle: &Value) -> Vec<Value> {
    let omissions = bundle["worker"]["omissions"].as_array().unwrap();
    omissions
        .iter()
        .filter(|omission| omission["name"].as_str().unwrap().starts_with("remote:"))
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn both_nested_remote_payloads_land_as_their_own_bundle_sections() {
    let harness = armed("evidence-nested").await;
    let mut command = capture_command();
    command.browser_evidence_json = browser_payload(&command.capture_id, &[], &[]).to_string();
    command.coordinator_evidence_json = coordinator_payload(&command.capture_id).to_string();
    let result = harness.capture(command).await;
    assert_eq!(result.error, None);
    let bundle = read_bundle(result.path.as_deref().unwrap()).await;
    for layer in ["browser", "coordinator"] {
        assert_eq!(bundle[layer]["layer"], json!(layer));
        assert_eq!(bundle[layer]["captured_at_ms"], json!(AT_MS));
    }
    assert!(remote_omissions(&bundle).is_empty());
    // Only the browser knows which invariant fired and how often.
    assert_eq!(
        bundle["trigger"]["detail"],
        json!("duplicate_history_index")
    );
    assert_eq!(
        (
            bundle["trigger"]["origin"].clone(),
            bundle["trigger"]["occurrence_count"].clone()
        ),
        (json!("browser"), json!(3))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nested_but_invalid_section_is_dropped_without_costing_worker_evidence() {
    let harness = armed("evidence-invalid").await;
    harness.deliver(b"worker-kept\r\n");
    let mut command = capture_command();
    command.browser_evidence_json = malformed_section_payload(&command.capture_id).to_string();
    let result = harness.capture(command).await;
    // Envelope and nesting were fine, so the capture succeeds; only the layer
    // that failed the write-side gate is absent.
    assert_eq!(
        (result.error, result.status),
        (None, TerminalCaptureStatus::Partial)
    );
    let bundle = read_bundle(result.path.as_deref().unwrap()).await;
    assert_eq!(bundle["browser"], Value::Null);
    let dropped = remote_omissions(&bundle);
    assert_eq!(dropped[0]["name"], json!("remote:browser.events"));
    assert_eq!(dropped[0]["reason"], json!("layer_unavailable"));
    assert!(!bundle["worker"]["raw"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_flattened_payload_with_no_nested_layer_member_is_refused_outright() {
    let harness = armed("evidence-flat").await;
    let mut command = capture_command();
    command.browser_evidence_json = flattened_browser_payload(&command.capture_id).to_string();
    let result = harness.capture(command).await;
    // Refused, not accepted-and-discarded: the layer must learn its shape is wrong.
    assert_eq!(
        (result.status, result.error, result.path),
        (
            TerminalCaptureStatus::Error,
            Some(TerminalCaptureErrorCode::EvidenceMalformed),
            None
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_peer_authored_trigger_is_dropped_not_paid_for_in_evidence() {
    let harness = armed("evidence-trigger").await;
    harness.deliver(b"worker-kept\r\n");
    let mut command = capture_command();
    command.browser_evidence_json = invalid_trigger_payload(&command.capture_id).to_string();
    let result = harness.capture(command).await;
    // An adopted invalid trigger would fail the gate AND its retry, losing the
    // worker's whole section; the retry falls back to the worker's own.
    assert_eq!(
        (result.error, result.status),
        (None, TerminalCaptureStatus::Partial)
    );
    let bundle = read_bundle(result.path.as_deref().unwrap()).await;
    let trigger = &bundle["trigger"];
    assert_eq!(
        (
            trigger["reason"].clone(),
            trigger["origin"].clone(),
            trigger["occurrence_count"].clone()
        ),
        (json!("manual"), json!("browser"), json!(0))
    );
    assert_eq!(bundle["browser"], Value::Null);
    assert!(!bundle["worker"]["raw"].as_array().unwrap().is_empty());
    assert!(!remote_omissions(&bundle).is_empty());
}
