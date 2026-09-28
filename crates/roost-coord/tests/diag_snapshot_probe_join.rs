//! The DiagSnapshot JOIN the web's `__smoke.terminalStreamProbe` performs,
//! end to end through the RPC: the keys it reads, in the shape it reads them,
//! and the two joins that would silently answer "healthy" if either side were
//! wrong.
//!
//! The probe lives in the web crate (`roost_web::smoke::stream_probe`, landed
//! on `v3-web` as `f17b305c`) and it normalizes the coordinator's
//! `snapshot_json` by KEY: `coord.sessions[sid].terminal_view`,
//! `coord.sessions[sid].route.worker_fp` -> `workers[fp]`, `coord.build`, and
//! the root's `captured_at_ms`. The two halves had never been checked against
//! each other, and a renamed or missing key does not fail a build -- it fills
//! a smoke probe with nulls. This is the coordinator's half of that check.
//!
//! WHAT MUST NEVER HAPPEN, because the probe reads each as healthy:
//!   * a `workers[fp]` envelope that says `ok` for a worker that did not answer;
//!   * a session id the coordinator does not hold resolving to another
//!     session's row.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "diag_snapshot_support/mod.rs"]
mod diag_snapshot_support;

use diag_snapshot_support::{
    DiagFixture, DiagReply, MISSING_SESSION, WORKER_C, batch_session_ids, device, sorted_keys,
};
use roost_coord::diagnostics::diag_snapshot::handle_diag_snapshot;
use roost_proto as proto;
use serde_json::Value;

fn request(session_filter_ids: &[String]) -> proto::DiagSnapshotRequest {
    proto::DiagSnapshotRequest {
        session_filter_id: String::new(),
        session_filter_ids: session_filter_ids.to_vec(),
        ..Default::default()
    }
}

async fn snapshot(fixture: &DiagFixture, session_filter_ids: &[String]) -> Value {
    let response = handle_diag_snapshot(
        &fixture.core,
        &device(),
        &fixture.pipelines,
        "coord-sha",
        request(session_filter_ids),
    )
    .await
    .expect("a snapshot");
    serde_json::from_str(&response.body.snapshot_json).expect("snapshot JSON")
}

/// The probe's own join, written out rather than described: the session's
/// route names a worker, and that worker is the key its envelope hangs under,
/// carrying that same session's row.
#[tokio::test]
async fn the_route_worker_is_the_key_its_envelope_hangs_under() {
    let mut fixture = DiagFixture::new("probe-join").await;
    fixture.connect_all_answering();
    let session_id = batch_session_ids()[0].clone();
    let snapshot = snapshot(&fixture, std::slice::from_ref(&session_id)).await;

    let worker_fp = snapshot["coord"]["sessions"][&session_id]["route"]["worker_fp"]
        .as_str()
        .expect("an admitted session names its worker")
        .to_owned();
    let envelope = &snapshot["workers"][&worker_fp];
    assert_eq!(envelope["status"], "ok", "the named worker answered");
    assert_eq!(envelope["snapshot"]["worker_fp"], worker_fp);
    assert!(
        envelope["response_ms"].is_number(),
        "the probe times the round trip it did not make: {}",
        envelope["response_ms"]
    );
    // The probe reads the worker's OWN row for the session it asked about; an
    // envelope that omitted it would read as a missing worker layer.
    assert!(
        envelope["snapshot"]["sessions"][&session_id].is_object(),
        "the worker's snapshot carries the session the probe asked about: {}",
        envelope["snapshot"]["sessions"]
    );
    assert_eq!(
        sorted_keys(&snapshot["workers"]),
        vec![worker_fp.clone()],
        "the dump reached no worker the route did not name"
    );
}

/// A session the coordinator does not hold has no row, so the probe's lookup
/// is a miss it reports as `terminal_control: null`. It must not resolve to a
/// neighbouring session's view.
#[tokio::test]
async fn a_session_the_coordinator_does_not_hold_has_no_row() {
    let mut fixture = DiagFixture::new("probe-absent").await;
    fixture.connect_all_answering();
    let held = batch_session_ids()[0].clone();
    let snapshot = snapshot(&fixture, &[held.clone(), MISSING_SESSION.to_owned()]).await;

    assert_eq!(
        sorted_keys(&snapshot["coord"]["sessions"]),
        std::slice::from_ref(&held),
        "an unknown id contributes no row, so the probe cannot read another's"
    );
    assert_eq!(
        snapshot["coord"]["sessions"][MISSING_SESSION],
        Value::Null,
        "an unknown id reads as absent, not as a borrowed session"
    );
}

/// The probe treats `status` as the worker's whole health. A worker that was
/// asked and never answered must read `error` with a code and a message, and
/// a worker the dump never reached must be absent -- which the probe reads as
/// `missing`. Neither may read `ok`.
#[tokio::test]
async fn a_worker_that_never_answered_is_never_reported_ok() {
    let mut fixture = DiagFixture::new("probe-silent").await;
    // The batch seeds even indexes on WORKER_A and odd on WORKER_C, so the
    // second session is the one WORKER_C owns and therefore the one it is
    // asked about.
    let silent_session = batch_session_ids()[1].clone();
    fixture.connect(WORKER_C, DiagReply::Silent);
    let snapshot = snapshot(&fixture, &[silent_session]).await;

    let silent = &snapshot["workers"][WORKER_C];
    assert_eq!(silent["status"], "error");
    assert!(
        silent["response_ms"].is_number(),
        "the failed round trip is still timed: {}",
        silent["response_ms"]
    );
    assert!(
        silent.get("snapshot").is_none(),
        "an error envelope carries no snapshot, or the probe reads a stale layer"
    );
    assert!(silent["error"]["code"].is_string());
    assert!(silent["error"]["message"].is_string());
    assert_eq!(
        sorted_keys(&snapshot["workers"]),
        vec![WORKER_C.to_owned()],
        "the dump reached no worker the route did not name"
    );
}

/// The root keys the probe takes as the coordinator's own identity and clock.
#[tokio::test]
async fn the_root_carries_the_build_and_the_clock_the_probe_reads() {
    let mut fixture = DiagFixture::new("probe-root").await;
    fixture.connect_all_answering();
    let session_id = batch_session_ids()[0].clone();
    let snapshot = snapshot(&fixture, &[session_id]).await;

    assert_eq!(snapshot["coord"]["build"]["git_sha"], "coord-sha");
    assert!(
        snapshot["coord"]["build"]["artifact_version"].is_string(),
        "an absent build string would read as an unidentified build, not a null: {}",
        snapshot["coord"]["build"]["artifact_version"]
    );
    assert!(
        snapshot["captured_at_ms"].is_u64() || snapshot["captured_at_ms"].is_i64(),
        "the probe's clock is a number: {}",
        snapshot["captured_at_ms"]
    );
}
