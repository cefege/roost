//! What the keeper-update preparation admits, and the proof it reports back.
//!
//! The admitted half of the decision, and the two ways a worker's answer can
//! still be refused: an outcome the requested action does not permit, and an
//! identity that does not describe a keeper. Drives the real handler over a
//! real database, with the socket answering the way the link's frame dispatcher
//! answers `rpc-ok`. The refusals that never reach a worker live in
//! `keeper_update_refusals.rs`; the drain's own properties in
//! `keeper_update_drain.rs`.

#[path = "keeper_update_support/mod.rs"]
mod keeper_support;
#[path = "workers_support/mod.rs"]
mod workers_support;

use std::sync::{Arc, Mutex};

use connectrpc::ErrorCode;
use keeper_support::{
    EPOCH, Observed, PrepareRequest, connect_observer, digest, enroll_device, journal, journaled,
    maintenance, observed_by, prepare,
};
use roost_coord::deploy::keeper_update::{
    KeeperUpdateAction, MAINTENANCE_ACTION, handle_workers_prepare_keeper_update,
};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde_json::json;
use workers_support::{WORKER_FP, WorkersFixture, device_caller};

#[tokio::test]
async fn a_preserved_keeper_is_reported_with_the_identity_that_holds_the_ptys() {
    let fixture = WorkersFixture::new("keeper-preserve").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "preserve-worker", 1_000)
        .await;
    let seen = Arc::new(Mutex::new(Observed::default()));
    connect_observer(
        &fixture,
        json!({
            "outcome": "preserved",
            "keeper_pid": 4242,
            "keeper_epoch": EPOCH,
            "binding_digest": digest("c"),
        }),
        Arc::clone(&seen),
    );

    let response = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, journaled("preserve", "source")),
    )
    .await
    .expect("an admissible preserve is admitted")
    .body;

    assert_eq!(response.outcome, "preserved");
    assert_eq!(response.keeper_pid, Some(4242));
    assert_eq!(response.keeper_epoch.as_deref(), Some(EPOCH));
    assert_eq!(
        response.binding_digest.as_deref(),
        Some(digest("c").as_str()),
    );

    // The worker learns the open-session proof so it can check it against the
    // channel count it observes on the host, and the journal it was sent is the
    // re-rendered one rather than the bytes the caller supplied.
    let observed = observed_by(&seen);
    match &observed.frames[0] {
        CoordWorkerDownstream::KeeperUpdatePrepare(body) => {
            assert_eq!(body.direction, "source");
            assert!(body.coordinator_open_session_ids.is_empty());
            assert_eq!(
                body.journaled_update_json.as_deref(),
                Some(journal("preserve").as_str()),
            );
        }
        other => panic!("a keeper update goes out as its own frame, not {other:?}"),
    }
}

#[tokio::test]
async fn an_authorized_maintenance_may_cross_a_live_session() {
    let fixture = WorkersFixture::new("keeper-force").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "force-worker", 1_000)
        .await;
    fixture
        .enroll_session(WORKER_FP, workers_support::SESSION_ID)
        .await;
    let seen = Arc::new(Mutex::new(Observed::default()));
    connect_observer(
        &fixture,
        json!({ "outcome": "shutdown" }),
        Arc::clone(&seen),
    );

    let response = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(
            WORKER_FP,
            PrepareRequest {
                force_live: true,
                ..maintenance()
            },
        ),
    )
    .await
    .expect("the operator authorized crossing live sessions")
    .body;

    assert_eq!(response.outcome, "shutdown");
    assert_eq!(response.keeper_pid, None, "a shutdown leaves no keeper");
    // The sessions it crossed are the ones the worker was shown, so the host can
    // check the count against what it observes.
    let observed = observed_by(&seen);
    match &observed.frames[0] {
        CoordWorkerDownstream::KeeperUpdatePrepare(body) => {
            assert!(body.force_live);
            assert_eq!(
                body.coordinator_open_session_ids,
                vec![workers_support::SESSION_ID]
            );
        }
        other => panic!("a keeper update goes out as its own frame, not {other:?}"),
    }
}

#[tokio::test]
async fn a_worker_proof_for_a_different_action_is_refused() {
    let fixture = WorkersFixture::new("keeper-mismatch").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "mismatch-worker", 1_000)
        .await;
    let seen = Arc::new(Mutex::new(Observed::default()));
    connect_observer(
        &fixture,
        json!({
            "outcome": "preserved",
            "keeper_pid": 4242,
            "keeper_epoch": EPOCH,
            "binding_digest": digest("c"),
        }),
        Arc::clone(&seen),
    );

    // A replace-empty asks the worker to shut the keeper down. A `preserved`
    // answer means no replacement happened, and reporting it as a success is
    // how a fleet's PTYs are lost.
    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, journaled("replace-empty", "target")),
    )
    .await
    .expect_err("a preserved answer satisfies no replacement");

    assert_eq!(
        refusal.message.as_deref(),
        Some("worker returned keeper proof for a different action"),
    );
    assert_eq!(refusal.code, ErrorCode::DataLoss);
}

#[tokio::test]
async fn a_preserved_keeper_must_name_the_process_holding_the_ptys() {
    let fixture = WorkersFixture::new("keeper-identity").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "identity-worker", 1_000)
        .await;
    let seen = Arc::new(Mutex::new(Observed::default()));
    connect_observer(
        &fixture,
        json!({ "outcome": "preserved", "keeper_pid": 4242 }),
        Arc::clone(&seen),
    );

    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, journaled("preserve", "source")),
    )
    .await
    .expect_err("a pid with no epoch and no binding names no keeper");

    assert_eq!(
        refusal.message.as_deref(),
        Some("worker returned malformed keeper identity"),
    );
    assert_eq!(refusal.code, ErrorCode::DataLoss);
}

#[tokio::test]
async fn a_shutdown_reports_no_keeper_because_none_is_left() {
    let fixture = WorkersFixture::new("keeper-noidentity").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "noidentity-worker", 1_000)
        .await;
    let seen = Arc::new(Mutex::new(Observed::default()));
    // An identity on a shutdown means the worker answered about a keeper this
    // call did not replace.
    connect_observer(
        &fixture,
        json!({
            "outcome": "shutdown",
            "keeper_pid": 4242,
            "keeper_epoch": EPOCH,
            "binding_digest": digest("c"),
        }),
        Arc::clone(&seen),
    );

    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, maintenance()),
    )
    .await
    .expect_err("a shutdown that names a keeper named the wrong one");

    assert_eq!(
        refusal.message.as_deref(),
        Some("worker returned unexpected keeper identity"),
    );
    assert_eq!(refusal.code, ErrorCode::DataLoss);
}

#[tokio::test]
async fn the_empty_keeper_requirement_follows_the_action_and_the_authorization() {
    // The predicate the whole slice rests on, at its boundary rather than
    // through a handler: a preserve never needs emptiness, a replace-empty
    // always does, and the maintenance path is the only one a flag moves.
    assert!(!KeeperUpdateAction::Preserve.requires_empty_keeper(false));
    assert!(KeeperUpdateAction::ReplaceEmpty.requires_empty_keeper(true));
    assert!(KeeperUpdateAction::Maintenance.requires_empty_keeper(false));
    assert!(!KeeperUpdateAction::Maintenance.requires_empty_keeper(true));
    assert_eq!(KeeperUpdateAction::Maintenance.as_str(), MAINTENANCE_ACTION);
    assert_eq!(KeeperUpdateAction::ReplaceEmpty.as_str(), "replace-empty");
    assert_eq!(KeeperUpdateAction::Preserve.as_str(), "preserve");
}
