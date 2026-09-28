//! The worker-RPC correlation table a spawn and an attach wait on: a dropped
//! worker socket fails exactly that worker's in-flight requests at once (the
//! browser's spawn spinner fast-fails instead of hanging to its deadline), and
//! request ids are namespaced by the authenticated worker.
//!
//! Ports `apps/coord/tests/pending-rpc-drop.test.ts` against
//! `terminal_screen::pending_rpcs::PendingRpcs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use connectrpc::ErrorCode;
use roost_coord::terminal_screen::pending_rpcs::PendingRpcs;
use serde_json::json;

const FP_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FP_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SHARED_UPLOAD_ID: &str = "shared-upload-id";

// v2: "rejects only the dropped worker's RPCs; others survive".
#[tokio::test]
async fn only_the_dropped_workers_rpcs_are_rejected() {
    let table = Arc::new(PendingRpcs::new());
    let mut dropped = table.create("rpc-a", Some(FP_A), 0).unwrap();
    let mut survivor = table.create("rpc-b", Some(FP_B), 0).unwrap();

    assert_eq!(table.reject_all_for_worker(FP_A, "worker disconnected"), 1);
    let error = dropped.settle().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Unavailable);
    assert!(
        error
            .message
            .as_deref()
            .unwrap()
            .contains("worker disconnected")
    );

    assert!(table.resolve("rpc-b", json!({ "ok": true }), Some(FP_B)));
    assert_eq!(survivor.settle().await.unwrap(), json!({ "ok": true }));
}

// v2: "rejecting clears the entries (no leak, no double-reject)".
#[tokio::test]
async fn a_rejection_clears_the_entries() {
    let table = Arc::new(PendingRpcs::new());
    let before = table.pending_count();
    let _pending = table.create("rpc-a", Some(FP_A), 0).unwrap();
    assert_eq!(table.pending_count(), before + 1);
    table.reject_all_for_worker(FP_A, "gone");
    assert_eq!(table.pending_count(), before);
    assert_eq!(table.reject_all_for_worker(FP_A, "gone again"), 0);
}

// v2: "untagged RPCs are not collateral".
#[tokio::test]
async fn an_untagged_rpc_survives_a_worker_drop() {
    let table = Arc::new(PendingRpcs::new());
    let mut untagged = table.create("rpc-untagged", None, 0).unwrap();
    assert_eq!(table.reject_all_for_worker(FP_A, "gone"), 0);
    assert!(table.resolve("rpc-untagged", json!(1), None));
    assert_eq!(untagged.settle().await.unwrap(), json!(1));
}

// v2: "equal upload ids resolve independently across workers".
#[tokio::test]
async fn equal_ids_resolve_independently_per_worker() {
    let table = Arc::new(PendingRpcs::new());
    let mut from_a = table.create(SHARED_UPLOAD_ID, Some(FP_A), 0).unwrap();
    let mut from_b = table.create(SHARED_UPLOAD_ID, Some(FP_B), 0).unwrap();
    assert!(table.resolve(SHARED_UPLOAD_ID, json!({ "worker": "b" }), Some(FP_B)));
    assert_eq!(from_b.settle().await.unwrap(), json!({ "worker": "b" }));
    assert!(table.resolve(SHARED_UPLOAD_ID, json!({ "worker": "a" }), Some(FP_A)));
    assert_eq!(from_a.settle().await.unwrap(), json!({ "worker": "a" }));
}

// v2: "duplicate explicit ids for one worker reject instead of overwriting".
#[tokio::test]
async fn a_duplicate_id_for_one_worker_is_refused() {
    let table = Arc::new(PendingRpcs::new());
    let _first = table.create(SHARED_UPLOAD_ID, Some(FP_A), 0).unwrap();
    let error = table.create(SHARED_UPLOAD_ID, Some(FP_A), 0).unwrap_err();
    assert_eq!(error.code, ErrorCode::AlreadyExists);
    assert_eq!(
        error.message.as_deref(),
        Some("request_id is already pending for this worker")
    );
}
