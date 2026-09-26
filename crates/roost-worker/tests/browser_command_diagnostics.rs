//! The two things a worker is willing to say about itself, and the one
//! rule both keep: no terminal text crosses the trust boundary.

mod browser_command_support;
use browser_command_support::{
    DIGEST, EPOCH, FINGERPRINT, Harness, HostPlatform, LocalFiles, MapEnv, OTHER_SESSION,
    SESSION, base64_decode, command, dispatch, every_kind, floor, frame_of, harness, only,
};
use roost_worker::browser_commands::Command;
use roost_worker::browser_commands::scrollback_page::GridDescription;
use roost_worker::browser_commands::search::Searches;

use roost_worker::scrollback_read::EpochBinding;
use std::sync::Arc;
use serde_json::json;

/// A SNAPSHOT ANSWERS WITH THE WORKER'S OWN STATE and nothing the terminal
/// produced.
#[tokio::test]
async fn a_snapshot_answers_with_the_state_report_and_no_terminal_text() {
    let harness = harness();
    let reply = only(dispatch(&command(frame_of("diag-snapshot")), &harness.deps).await);
    let data = reply.data().expect("a snapshot answers with data");
    assert_eq!(data["channels"], json!([]));
    assert!(data["captured_at_ms"].is_number());
}

/// EVERY CAPTURE STEP IS ANSWERED, AND A FAILURE IS A FIXED CODE. A parser
/// message from a grid walk quotes the cells it failed on, and this answer
/// becomes an operator-visible download.
#[tokio::test]
async fn every_capture_step_is_answered_with_a_fixed_vocabulary() {
    let harness = harness();
    let reply = only(dispatch(&command(frame_of("diag-terminal-capture")), &harness.deps).await);
    let data = reply
        .data()
        .expect("a capture answers with its acknowledgement");
    assert_eq!(data["status"], json!("captured"));
    assert_eq!(data["error"], json!(null));
    assert_eq!(
        data["recent_worker_capture"]["capture_id"],
        json!("3f6b2a10-0000-4000-8000-0000000000ff")
    );
    assert!(harness.diagnostics.started.lock().expect("held").is_empty());
    assert!(harness.diagnostics.stopped.lock().expect("held").is_empty());
}
