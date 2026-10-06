//! The `diag-snapshot` worker fan-out and the per-worker result scoping: one
//! answer and one timeout side by side, a worker that is not connected, a
//! reply that is not an object, and the envelope a DiagSnapshot carries.
//!
//! Ports `apps/coord/tests/diagnostics/diag-snapshot-fanout.test.ts` and the
//! scoping of `apps/coord/src/diagnostics/diag-snapshot-worker-results.ts`.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
#[path = "diag_snapshot_support/mod.rs"]
mod diag_snapshot_support;

use std::collections::HashSet;

use diag_snapshot_support::{DiagFixture, DiagReply, WORKER_A, WORKER_C, WORKER_LOCAL};
use roost_coord::diagnostics::worker_results::scoped_worker_diagnostic;
use roost_coord::workers::diag_send::{
    WorkerDiagSnapshotErrorCode, WorkerDiagSnapshotResult, collect_worker_diag_snapshots,
};
use roost_protocol::wire::WorkerFp;
use serde_json::{Value, json};

fn fp(value: &str) -> WorkerFp {
    WorkerFp::try_from(value).unwrap()
}

#[tokio::test]
async fn returns_one_response_and_one_explicit_timeout_without_leaking_a_pending_rpc() {
    let mut fixture = DiagFixture::new("fanout-timeout").await;
    let body =
        json!({ "build": { "git_sha": "worker-build" }, "worker_fp": WORKER_A, "sessions": {} });
    fixture.connect(WORKER_A, DiagReply::Answer(body.clone()));
    fixture.connect(WORKER_C, DiagReply::Silent);
    let relay = fixture.core.services.scrollback.clone();

    let result = collect_worker_diag_snapshots(&relay, [fp(WORKER_C), fp(WORKER_A)], 20).await;

    let keys: Vec<&str> = result.keys().map(WorkerFp::as_str).collect();
    assert_eq!(keys, [WORKER_A, WORKER_C]);
    let WorkerDiagSnapshotResult::Ok { snapshot, .. } = &result[&fp(WORKER_A)] else {
        panic!("the answering worker is ok: {:?}", result[&fp(WORKER_A)]);
    };
    assert_eq!(Value::Object(snapshot.clone()), body);
    let WorkerDiagSnapshotResult::Error { code, message, .. } = &result[&fp(WORKER_C)] else {
        panic!("the silent worker is an error");
    };
    assert_eq!(*code, WorkerDiagSnapshotErrorCode::Timeout);
    assert!(message.contains("did not reply"), "{message}");

    // The timeout released its correlation entry: a late authenticated reply,
    // and a spoofed one from another worker, both find nothing to settle.
    let late_id = fixture.log().diag_request_ids[WORKER_C].clone();
    assert!(
        !relay
            .pending()
            .resolve(&late_id, json!({ "late": true }), Some(WORKER_C))
    );
    assert!(
        !relay
            .pending()
            .resolve(&late_id, json!({ "spoofed": true }), Some(WORKER_A))
    );
}

#[tokio::test]
async fn reports_a_registered_but_disconnected_worker_as_offline() {
    let fixture = DiagFixture::new("fanout-offline").await;
    let relay = fixture.core.services.scrollback.clone();
    let result = collect_worker_diag_snapshots(&relay, [fp(WORKER_LOCAL)], 20).await;
    assert_eq!(
        result[&fp(WORKER_LOCAL)].to_json()["error"],
        json!({ "code": "offline", "message": "worker is not connected" })
    );
    assert_eq!(result[&fp(WORKER_LOCAL)].to_json()["status"], "error");
}

#[tokio::test]
async fn a_revoked_generation_is_offline_and_never_sent_to() {
    let mut fixture = DiagFixture::new("fanout-revoked").await;
    fixture
        .connect(WORKER_A, DiagReply::Answer(json!({})))
        .revoke();
    let relay = fixture.core.services.scrollback.clone();
    let result = collect_worker_diag_snapshots(&relay, [fp(WORKER_A)], 20).await;
    assert_eq!(result[&fp(WORKER_A)].to_json()["error"]["code"], "offline");
    assert!(fixture.log().sent_worker_fps.is_empty());
}

#[tokio::test]
async fn a_reply_that_is_not_an_object_is_an_rpc_error() {
    let mut fixture = DiagFixture::new("fanout-invalid").await;
    fixture.connect(WORKER_A, DiagReply::Answer(json!(["not", "an", "object"])));
    let relay = fixture.core.services.scrollback.clone();
    let result = collect_worker_diag_snapshots(&relay, [fp(WORKER_A)], 20).await;
    assert_eq!(
        result[&fp(WORKER_A)].to_json()["error"],
        json!({ "code": "rpc_error", "message": "worker returned an invalid diagnostic snapshot" })
    );
}

#[test]
fn scoping_keeps_only_admitted_sessions_and_the_authenticated_fingerprint() {
    let answered = WorkerDiagSnapshotResult::Ok {
        response_ms: 3,
        snapshot: json!({
            "captured_at_ms": 9,
            "build": { "git_sha": "w" },
            "worker_fp": "spoofed",
            "sessions": { "allowed": { "x": 1 }, "other": { "x": 2 } },
            "extra": true,
        })
        .as_object()
        .unwrap()
        .clone(),
    };
    let allowed: HashSet<String> = ["allowed".to_owned()].into();
    assert_eq!(
        scoped_worker_diagnostic(&fp(WORKER_A), &answered, &allowed),
        json!({
            "status": "ok",
            "response_ms": 3,
            "snapshot": {
                "captured_at_ms": 9,
                "build": { "git_sha": "w" },
                "worker_fp": WORKER_A,
                "sessions": { "allowed": { "x": 1 } },
            },
        })
    );

    // A snapshot whose `sessions` is not a map scopes to an empty map, and an
    // absent `build` stays absent rather than becoming null.
    let odd = WorkerDiagSnapshotResult::Ok {
        response_ms: 1,
        snapshot: json!({ "sessions": ["allowed"] })
            .as_object()
            .unwrap()
            .clone(),
    };
    let scoped = scoped_worker_diagnostic(&fp(WORKER_A), &odd, &allowed);
    assert_eq!(scoped["snapshot"]["sessions"], json!({}));
    assert!(scoped["snapshot"].get("build").is_none());
}
