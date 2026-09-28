//! `CoordinatorService.DiagSnapshot` end to end: who may ask, how the two
//! session filter forms normalize, which workers a dump reaches, the capped
//! unfiltered fleet dump, and the terminal-capture scope refusal.
//!
//! Ports `apps/coord/tests/diagnostics/diag-snapshot-handlers.test.ts`
//! ("DiagSnapshot session filters" and the capture scope and auth cases of
//! "DiagSnapshot terminal capture dispatch").
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "diag_snapshot_support/mod.rs"]
mod diag_snapshot_support;

use connectrpc::{ConnectError, ErrorCode};
use diag_snapshot_support::{
    DiagFixture, LOCAL_UNSELECTED_SESSION, MISSING_SESSION, WORKER_A, WORKER_C, WORKER_LOCAL,
    batch_session_ids, device, returned_pipeline_session_ids, returned_session_ids, sorted_keys,
    worker_caller,
};
use roost_coord::coord_core::Caller;
use roost_coord::diagnostics::diag_snapshot::handle_diag_snapshot;
use roost_proto as proto;
use serde_json::Value;

fn request(filter_id: &str, filter_ids: &[String]) -> proto::DiagSnapshotRequest {
    proto::DiagSnapshotRequest {
        session_filter_id: filter_id.to_owned(),
        session_filter_ids: filter_ids.to_vec(),
        ..Default::default()
    }
}

async fn snapshot_as(
    fixture: &DiagFixture,
    caller: &Caller,
    request: proto::DiagSnapshotRequest,
) -> Result<Value, ConnectError> {
    let response = handle_diag_snapshot(
        &fixture.core,
        caller,
        &fixture.pipelines,
        "coord-sha",
        request,
    )
    .await?;
    Ok(serde_json::from_str(&response.body.snapshot_json).unwrap())
}

async fn snapshot(fixture: &DiagFixture, request: proto::DiagSnapshotRequest) -> Value {
    snapshot_as(fixture, &device(), request)
        .await
        .expect("a snapshot")
}

async fn answering_fixture(label: &str) -> DiagFixture {
    let mut fixture = DiagFixture::new(label).await;
    fixture.connect_all_answering();
    fixture
}

#[tokio::test]
async fn keeps_singular_session_filter_id_compatibility() {
    let fixture = answering_fixture("singular").await;
    let session_id = batch_session_ids()[0].clone();
    let snapshot = snapshot(&fixture, request(&session_id, &[])).await;

    assert_eq!(
        sorted_keys(&snapshot["coord"]["sessions"]),
        std::slice::from_ref(&session_id)
    );
    assert_eq!(sorted_keys(&snapshot["workers"]), [WORKER_A]);
    assert_eq!(
        returned_session_ids(&snapshot),
        std::slice::from_ref(&session_id)
    );
    assert_eq!(fixture.log().sent_worker_fps, [WORKER_A]);
    assert_eq!(
        returned_pipeline_session_ids(&snapshot),
        std::slice::from_ref(&session_id)
    );
    assert_eq!(
        fixture.log().pipeline_targets.get(WORKER_A),
        Some(&vec![session_id])
    );
    // The worker's own fields are cut to the scoped shape: the authenticated
    // fingerprint replaces the payload's, and unknown keys never pass.
    let envelope = &snapshot["workers"][WORKER_A];
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["snapshot"]["worker_fp"], WORKER_A);
    assert_eq!(envelope["snapshot"]["build"]["git_sha"], "worker-build");
    assert!(envelope["snapshot"].get("secrets").is_none());
    assert_eq!(snapshot["coord"]["build"]["git_sha"], "coord-sha");
    assert!(snapshot.get("truncated").is_none());
}

#[tokio::test]
async fn admits_64_local_ids_and_targets_only_their_workers() {
    let fixture = answering_fixture("batch").await;
    let mut batch = batch_session_ids();
    let snapshot = snapshot(&fixture, request("", &batch)).await;
    batch.sort();

    assert_eq!(sorted_keys(&snapshot["coord"]["sessions"]), batch);
    assert_eq!(sorted_keys(&snapshot["workers"]), [WORKER_A, WORKER_C]);
    assert_eq!(returned_session_ids(&snapshot), batch);
    let mut sent = fixture.log().sent_worker_fps.clone();
    sent.sort();
    assert_eq!(sent, [WORKER_A, WORKER_C]);
    assert_eq!(returned_pipeline_session_ids(&snapshot), batch);
    let targeted: Vec<String> = fixture.log().pipeline_targets.keys().cloned().collect();
    assert_eq!(targeted, [WORKER_A, WORKER_C]);
}

#[tokio::test]
async fn excludes_unknown_batch_ids_and_unrelated_workers() {
    let fixture = answering_fixture("unknown").await;
    let local = batch_session_ids()[0].clone();
    let snapshot = snapshot(
        &fixture,
        request("", &[local.clone(), MISSING_SESSION.to_owned()]),
    )
    .await;

    assert_eq!(
        sorted_keys(&snapshot["coord"]["sessions"]),
        std::slice::from_ref(&local)
    );
    assert_eq!(sorted_keys(&snapshot["workers"]), [WORKER_A]);
    assert_eq!(
        returned_session_ids(&snapshot),
        std::slice::from_ref(&local)
    );
    assert_eq!(fixture.log().sent_worker_fps, [WORKER_A]);
    assert_eq!(returned_pipeline_session_ids(&snapshot), [local]);
    assert!(snapshot["workers"].get(WORKER_LOCAL).is_none());
}

#[tokio::test]
async fn rejects_oversized_and_ambiguous_filter_input_before_dispatch() {
    let fixture = answering_fixture("reject").await;
    let batch = batch_session_ids();
    let first = batch[0].clone();
    let mut oversized = batch.clone();
    oversized.push(LOCAL_UNSELECTED_SESSION.to_owned());
    for bad in [
        request("", &oversized),
        request(&first, std::slice::from_ref(&first)),
        request("", &[first.clone(), first.clone()]),
        request("", &[String::new()]),
    ] {
        let error = snapshot_as(&fixture, &device(), bad).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }
    assert!(fixture.log().sent_worker_fps.is_empty());
    assert!(fixture.log().pipeline_targets.is_empty());
}

#[tokio::test]
async fn caps_the_unfiltered_fleet_dump_and_marks_it_truncated() {
    let fixture = answering_fixture("cap").await;
    let snapshot = snapshot(&fixture, request("", &[])).await;

    let mut open = batch_session_ids();
    open.push(LOCAL_UNSELECTED_SESSION.to_owned());
    let returned = sorted_keys(&snapshot["coord"]["sessions"]);
    assert_eq!(returned.len(), 64);
    assert!(returned.iter().all(|session_id| open.contains(session_id)));
    assert_eq!(snapshot["truncated"], true);
    assert_eq!(
        sorted_keys(&snapshot["workers"]),
        [WORKER_A, WORKER_C, WORKER_LOCAL]
    );
    let mut sent = fixture.log().sent_worker_fps.clone();
    sent.sort();
    assert_eq!(sent, [WORKER_A, WORKER_C, WORKER_LOCAL]);
    assert_eq!(
        returned_pipeline_session_ids(&snapshot).len(),
        returned.len()
    );
}

#[tokio::test]
async fn spa_state_is_carried_and_malformed_spa_state_is_null() {
    let fixture = answering_fixture("spa").await;
    let session_id = batch_session_ids()[0].clone();
    let mut carried = request(&session_id, &[]);
    carried.spa_state_json = r#"{"route":"sync"}"#.to_owned();
    assert_eq!(snapshot(&fixture, carried).await["spa"]["route"], "sync");
    let mut malformed = request(&session_id, &[]);
    malformed.spa_state_json = "{not-json".to_owned();
    assert_eq!(snapshot(&fixture, malformed).await["spa"], Value::Null);
}

#[tokio::test]
async fn capture_requires_exactly_one_session_filter_ids_entry_naming_the_capture_session() {
    let fixture = answering_fixture("capture").await;
    let batch = batch_session_ids();
    let capture_session = batch[0].clone();
    let capture = |filter_id: &str, filter_ids: &[String]| {
        let mut request = request(filter_id, filter_ids);
        request.terminal_capture = proto::TerminalCaptureRequest {
            session_id: capture_session.clone(),
            ..Default::default()
        }
        .into();
        request
    };
    for bad in [
        capture("", &[]),
        capture("", &[MISSING_SESSION.to_owned()]),
        capture("", &[capture_session.clone(), batch[1].clone()]),
        capture(&capture_session, &[]),
    ] {
        let error = snapshot_as(&fixture, &device(), bad).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }
    // A well-scoped capture is refused by name: the capture bridge is not
    // ported, and an ordinary dump would be a wrong answer.
    let scoped = capture("", std::slice::from_ref(&capture_session));
    let error = snapshot_as(&fixture, &device(), scoped).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Unimplemented);
    assert!(fixture.log().sent_worker_fps.is_empty());
}

#[tokio::test]
async fn rejects_a_caller_that_is_not_a_paired_device_before_any_work() {
    let fixture = answering_fixture("auth").await;
    let session_id = batch_session_ids()[0].clone();
    let error = snapshot_as(&fixture, &worker_caller(), request(&session_id, &[]))
        .await
        .unwrap_err();
    assert!(matches!(
        error.code,
        ErrorCode::Unauthenticated | ErrorCode::PermissionDenied
    ));
    assert!(fixture.log().sent_worker_fps.is_empty());
    assert!(fixture.log().pipeline_targets.is_empty());
}
