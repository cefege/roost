//! The coordinator's keeper-update preparation at the worker: admission closes
//! in the call, the coordinator's session set must equal the worker's, a
//! journaled preparation that succeeds stays closed, and every failure — and a
//! completed maintenance shutdown — reopens admission and releases the reconcile
//! boundary. Ports the handler cases of
//! `apps/worker/tests/session/session-channel-creation-gate.test.ts` and the
//! request validation of `apps/worker/src/transport/coord-link-keeper-update.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "keeper_update_prepare_support/mod.rs"]
mod keeper_update_prepare_support;
#[path = "keeper_update_support/mod.rs"]
mod keeper_update_support;
#[path = "session_support/mod.rs"]
mod session_support;

use std::sync::Mutex;
use std::sync::atomic::Ordering;

use keeper_update_prepare_support::{
    ACTIVE, Actions, CHANNEL, Fixture, fixture, journaled, maintenance,
};
use roost_worker::keeper_pool::{JournaledKeeperUpdateActionV1, UpdateDirection};
use roost_worker::link_ports::KeeperUpdatePort;
use roost_worker::session::input_write::WorkerInputResult;
use roost_worker::session::keeper_admission::KEEPER_UPDATE_WRITE_REFUSAL;
use session_support::{SESSION, session_id};
use tokio::sync::oneshot;

async fn input(fixture: &Fixture, seq: u64) -> WorkerInputResult {
    fixture
        .harness
        .manager
        .write_terminal_input(&session_id(SESSION), seq, b"x".to_vec(), None, None)
        .await
}

fn frozen() -> WorkerInputResult {
    WorkerInputResult::Rejected {
        reason: KEEPER_UPDATE_WRITE_REFUSAL.to_owned(),
    }
}

#[tokio::test]
async fn a_journaled_preparation_that_succeeds_stays_closed_and_holds_the_boundary() {
    let fixture = fixture(Actions::default());
    let answer = fixture
        .preparer
        .prepare(journaled(&[SESSION]))
        .await
        .unwrap();
    assert_eq!(answer["outcome"], "preserved");
    assert_eq!(answer["keeper_pid"], 4242);
    let applied = fixture.actions.applied.lock().unwrap().clone();
    assert_eq!(
        applied,
        vec![JournaledKeeperUpdateActionV1 {
            schema_version: 1,
            update: keeper_update_support::update(true, &ACTIVE),
            direction: UpdateDirection::Target,
            coordinator_open_session_ids: vec![SESSION.to_owned()],
            worker_open_channel_ids: vec![u32::from(CHANNEL)],
        }]
    );
    assert_eq!(fixture.boundary.acquired.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.boundary.released.load(Ordering::SeqCst),
        0,
        "the boundary stays held"
    );
    assert!(
        fixture.harness.manager.keeper_update_prepared(),
        "admission stays closed"
    );
    assert_eq!(input(&fixture, 1).await, frozen());
    assert!(fixture.harness.keeper.input.written().is_empty());
}

#[tokio::test]
async fn rejects_a_coordinator_session_set_that_differs_from_live_worker_state() {
    let fixture = fixture(Actions::default());
    let error = fixture.preparer.prepare(journaled(&[])).await.unwrap_err();
    assert!(
        error.contains("coordinator and worker open sessions changed"),
        "{error}"
    );
    assert!(fixture.actions.applied.lock().unwrap().is_empty());
    assert_eq!(fixture.boundary.released.load(Ordering::SeqCst), 1);
    assert!(
        !fixture.harness.manager.keeper_update_prepared(),
        "admission reopened"
    );
    assert_eq!(
        input(&fixture, 1).await,
        WorkerInputResult::Accepted { written_bytes: 1 }
    );
}

#[tokio::test]
async fn a_failed_update_action_reopens_admission_and_releases_the_boundary() {
    let fixture = fixture(Actions {
        apply_fails: true,
        ..Actions::default()
    });
    let error = fixture
        .preparer
        .prepare(journaled(&[SESSION]))
        .await
        .unwrap_err();
    assert_eq!(error, "injected keeper update failure");
    assert_eq!(fixture.boundary.released.load(Ordering::SeqCst), 1);
    assert!(!fixture.harness.manager.keeper_update_prepared());
}

#[tokio::test]
async fn malformed_requests_are_refused_before_any_keeper_action() {
    let fixture = fixture(Actions::default());
    let mut with_journal = maintenance(true, &[SESSION]);
    with_journal.journaled_update_json = journaled(&[]).journaled_update_json;
    let mut with_direction = maintenance(true, &[SESSION]);
    with_direction.direction = "target".to_owned();
    let unforced_live = maintenance(false, &[SESSION]);
    for request in [with_journal, with_direction, unforced_live] {
        let error = fixture.preparer.prepare(request).await.unwrap_err();
        assert_eq!(error, "keeper maintenance request is malformed");
    }
    let mut forced_journal = journaled(&[SESSION]);
    forced_journal.force_live = true;
    let mut no_direction = journaled(&[SESSION]);
    no_direction.direction = String::new();
    let mut no_journal = journaled(&[SESSION]);
    no_journal.journaled_update_json = None;
    for request in [forced_journal, no_direction, no_journal] {
        let error = fixture.preparer.prepare(request).await.unwrap_err();
        assert_eq!(error, "journaled keeper update request is malformed");
    }
    assert!(fixture.actions.applied.lock().unwrap().is_empty());
    assert_eq!(fixture.actions.maintenance_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.boundary.released.load(Ordering::SeqCst), 6);
    assert!(!fixture.harness.manager.keeper_update_prepared());
}

#[tokio::test]
async fn a_completed_maintenance_shutdown_reopens_admission_and_releases_the_boundary() {
    let fixture = fixture(Actions::default());
    let answer = fixture
        .preparer
        .prepare(maintenance(true, &[SESSION]))
        .await
        .unwrap();
    assert_eq!(answer, serde_json::json!({ "outcome": "shutdown" }));
    assert_eq!(fixture.boundary.released.load(Ordering::SeqCst), 1);
    assert!(!fixture.harness.manager.keeper_update_prepared());
}

#[tokio::test]
async fn preparations_run_one_at_a_time_in_arrival_order() {
    let (answer, deferred) = oneshot::channel();
    let fixture = fixture(Actions {
        deferred: Mutex::new(Some(deferred)),
        ..Actions::default()
    });
    let first = tokio::spawn(fixture.preparer.prepare(maintenance(true, &[SESSION])));
    let second = tokio::spawn(fixture.preparer.prepare(maintenance(true, &[SESSION])));
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        fixture.boundary.acquired.load(Ordering::SeqCst),
        1,
        "the second waits its turn"
    );
    answer.send(Ok("shutdown")).unwrap();
    assert!(first.await.unwrap().is_ok());
    assert!(second.await.unwrap().is_ok());
    assert_eq!(fixture.boundary.acquired.load(Ordering::SeqCst), 2);
    assert!(
        !fixture.harness.manager.keeper_update_prepared(),
        "both preparations released admission"
    );
}
