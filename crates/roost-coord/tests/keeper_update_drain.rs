//! The write drain a keeper update holds, and the writes it excludes.
//!
//! The emptiness proof a keeper update acts on is only worth anything while
//! nothing can create a channel between the coordinator reading the session
//! list and the worker shutting the keeper down. These are the properties that
//! make that true, checked at the two places they can fail: the handler must
//! take the drain, and the decision must refuse to run without one.

// `unwrap_used` and `expect_used` are denied outside `#[cfg(test)]`, and an
// integration test is its own crate rather than a module of one, so the
// exemption has to be stated here rather than inherited. Every panic below
// is an assertion over a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
#[path = "keeper_update_support/mod.rs"]
mod keeper_support;
#[path = "workers_support/mod.rs"]
mod workers_support;

use std::sync::{Arc, Mutex};

use connectrpc::ErrorCode;
use keeper_support::{
    Observed, connect_observer, enroll_device, maintenance, observed_by, prepare,
};
use roost_coord::deploy::keeper_update::{
    KeeperUpdateRefusal, KeeperUpdateRequest, decide_keeper_update,
    handle_workers_prepare_keeper_update,
};
use roost_coord::write_gate::WriteGate;
use roost_protocol::wire::WorkerFp;
use serde_json::json;
use workers_support::{WORKER_FP, WorkersFixture, device_caller};

#[tokio::test]
async fn the_drain_is_held_across_the_decision_and_excludes_a_write() {
    let fixture = WorkersFixture::new("keeper-drain").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "drain-worker", 1_000)
        .await;
    let seen = Arc::new(Mutex::new(Observed::default()));
    connect_observer(
        &fixture,
        json!({ "outcome": "already-absent" }),
        Arc::clone(&seen),
    );

    let response = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, maintenance()),
    )
    .await
    .expect("an empty keeper shuts down")
    .body;
    assert_eq!(response.outcome, "already-absent");

    let observed = observed_by(&seen);
    assert_eq!(
        observed.exclusive_held,
        vec![true],
        "the frame reaches the worker inside the drain, not before it",
    );
    assert_eq!(
        observed.write_refused,
        vec![true],
        "a durable mutation is excluded while the keeper update decides",
    );
    // And the gate reopens once the decision is done, or the coordinator would
    // never accept another mutation again.
    assert!(
        fixture.core.services.write_gate().acquire_shared().is_ok(),
        "the drain is released when the preparation finishes",
    );
}

#[tokio::test]
async fn a_decision_reached_outside_the_drain_is_refused() {
    let fixture = WorkersFixture::new("keeper-undrained").await;
    fixture
        .enroll_worker(WORKER_FP, "undrained-worker", 1_000)
        .await;
    let request = KeeperUpdateRequest::parse(maintenance()).expect("a maintenance request parses");

    let refusal = decide_keeper_update(
        &WriteGate::new(),
        &fixture.database,
        &WorkerFp::try_from(WORKER_FP).expect("a fingerprint the brand accepts"),
        &request,
    )
    .await
    .expect_err("an emptiness proof reached with the drain released proves nothing");

    assert_eq!(refusal, KeeperUpdateRefusal::DrainNotHeld);
    assert_eq!(refusal.code(), ErrorCode::Internal);
}

#[tokio::test]
async fn a_decision_reached_inside_the_drain_is_not_refused_for_the_drain() {
    let fixture = WorkersFixture::new("keeper-drained").await;
    fixture
        .enroll_worker(WORKER_FP, "drained-worker", 1_000)
        .await;
    let gate = WriteGate::new();
    let _drain = gate.acquire_exclusive().expect("the drain is free");
    let request = KeeperUpdateRequest::parse(maintenance()).expect("a maintenance request parses");

    let admission = decide_keeper_update(
        &gate,
        &fixture.database,
        &WorkerFp::try_from(WORKER_FP).expect("a fingerprint the brand accepts"),
        &request,
    )
    .await
    .expect("inside the drain the emptiness proof is admissible");

    assert!(admission.open_session_ids.is_empty());
    assert!(admission.maintenance);
    assert!(!admission.force_live);
}

#[tokio::test]
async fn a_second_preparation_is_refused_while_the_drain_is_held() {
    let fixture = WorkersFixture::new("keeper-second").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "second-worker", 1_000)
        .await;
    let held = fixture.core.services.write_gate().acquire_exclusive();
    assert!(held.is_ok(), "the first preparation took the drain");

    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, maintenance()),
    )
    .await
    .expect_err("a second handover may not overlap the first");

    assert_eq!(
        refusal.message.as_deref(),
        Some("coordinator keeper update preparation is held"),
    );
    assert_eq!(refusal.code, ErrorCode::Unavailable);
    // A refusal that never reached the machine sends nothing to it.
    assert!(
        fixture.core.services.scrollback.pending().pending_count() == 0,
        "a refused preparation leaves no correlation entry behind",
    );
}
