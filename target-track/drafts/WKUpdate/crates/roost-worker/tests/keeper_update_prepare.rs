//! The coordinator's keeper-update preparation at the worker: admission closes
//! in the call, the coordinator's session set must equal the worker's, a
//! journaled preparation that succeeds stays closed, and every failure — and a
//! completed maintenance shutdown — reopens admission and releases the reconcile
//! boundary. Ports the handler cases of
//! `apps/worker/tests/session/session-channel-creation-gate.test.ts` and the
//! request validation of `apps/worker/src/transport/coord-link-keeper-update.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "session_support/mod.rs"]
mod session_support;
#[path = "keeper_update_support/mod.rs"]
mod keeper_update_support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use roost_proto::DKeeperUpdatePrepare;
use roost_protocol::keeper_update::KeeperBinding;
use roost_worker::keeper_pool::{
    BoundaryRelease, JournaledKeeperUpdateActionV1, KeeperUpdateActionResult, KeeperUpdateActions,
    KeeperUpdateBoundary, KeeperUpdatePreparer, UpdateDirection,
};
use roost_worker::link_ports::KeeperUpdatePort;
use roost_worker::session::input_write::WorkerInputResult;
use roost_worker::session::keeper_admission::KEEPER_UPDATE_WRITE_REFUSAL;
use roost_worker::uplink::OwnerFuture;
use session_support::{Harness, SESSION, session_id};
use tokio::sync::oneshot;

const CHANNEL: u16 = 7;
const ACTIVE: [KeeperBinding; 1] = [KeeperBinding { channel_id: 7, pid: 5252 }];

#[derive(Default)]
struct Boundary {
    acquired: AtomicUsize,
    released: Arc<AtomicUsize>,
}

impl KeeperUpdateBoundary for Boundary {
    fn acquire(&self) -> OwnerFuture<Result<BoundaryRelease, String>> {
        self.acquired.fetch_add(1, Ordering::SeqCst);
        let released = Arc::clone(&self.released);
        Box::pin(async move {
            Ok(Box::new(move || {
                released.fetch_add(1, Ordering::SeqCst);
            }) as BoundaryRelease)
        })
    }
}

/// v2's injected `applyKeeperUpdateAction` / `shutdownKeeperForMaintenance`.
#[derive(Default)]
struct Actions {
    applied: Mutex<Vec<JournaledKeeperUpdateActionV1>>,
    apply_fails: bool,
    maintenance_calls: AtomicUsize,
    /// When set, the maintenance shutdown waits for this answer.
    deferred: Mutex<Option<oneshot::Receiver<Result<&'static str, String>>>>,
}

impl KeeperUpdateActions for Actions {
    fn apply(
        &self,
        action: JournaledKeeperUpdateActionV1,
    ) -> OwnerFuture<Result<KeeperUpdateActionResult, String>> {
        self.applied.lock().unwrap().push(action);
        let fails = self.apply_fails;
        Box::pin(async move {
            if fails {
                return Err("injected keeper update failure".to_owned());
            }
            Ok(KeeperUpdateActionResult {
                outcome: "preserved",
                keeper_pid: Some(4242),
                keeper_epoch: Some(keeper_update_support::KEEPER_EPOCH.to_owned()),
                binding_digest: Some(keeper_update_support::digest_of(&ACTIVE)),
            })
        })
    }

    fn maintenance_shutdown(&self, _force_live: bool) -> OwnerFuture<Result<&'static str, String>> {
        self.maintenance_calls.fetch_add(1, Ordering::SeqCst);
        let deferred = self.deferred.lock().unwrap().take();
        Box::pin(async move {
            match deferred {
                Some(answer) => answer.await.unwrap_or_else(|_| Err("dropped".to_owned())),
                None => Ok("shutdown"),
            }
        })
    }
}

struct Fixture {
    harness: Harness,
    boundary: Arc<Boundary>,
    actions: Arc<Actions>,
    preparer: Arc<KeeperUpdatePreparer>,
}

fn fixture(actions: Actions) -> Fixture {
    let harness = Harness::new();
    harness.install(SESSION, CHANNEL, "/home/user/project", "/home/user/project");
    let boundary = Arc::new(Boundary::default());
    let actions = Arc::new(actions);
    let preparer = Arc::new(KeeperUpdatePreparer::new(
        Arc::clone(&harness.manager),
        Arc::clone(&harness.table),
        Arc::clone(&boundary) as Arc<dyn KeeperUpdateBoundary>,
        Arc::clone(&actions) as Arc<dyn KeeperUpdateActions>,
    ));
    Fixture { harness, boundary, actions, preparer }
}

fn journaled(sessions: &[&str]) -> DKeeperUpdatePrepare {
    DKeeperUpdatePrepare {
        request_id: "keeper-update-prepare".to_owned(),
        journaled_update_json: Some(
            serde_json::to_string(&keeper_update_support::update(true, &ACTIVE)).unwrap(),
        ),
        direction: "target".to_owned(),
        maintenance: false,
        coordinator_open_session_ids: sessions.iter().map(|id| (*id).to_owned()).collect(),
        force_live: false,
        ..Default::default()
    }
}

fn maintenance(force_live: bool, sessions: &[&str]) -> DKeeperUpdatePrepare {
    DKeeperUpdatePrepare {
        request_id: "keeper-maintenance".to_owned(),
        journaled_update_json: None,
        direction: String::new(),
        maintenance: true,
        coordinator_open_session_ids: sessions.iter().map(|id| (*id).to_owned()).collect(),
        force_live,
        ..Default::default()
    }
}

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
    let answer = fixture.preparer.prepare(journaled(&[SESSION])).await.unwrap();
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
    assert_eq!(fixture.boundary.released.load(Ordering::SeqCst), 0, "the boundary stays held");
    assert!(fixture.harness.manager.keeper_update_prepared(), "admission stays closed");
    assert_eq!(input(&fixture, 1).await, frozen());
    assert!(fixture.harness.keeper.input.written().is_empty());
}

#[tokio::test]
async fn rejects_a_coordinator_session_set_that_differs_from_live_worker_state() {
    let fixture = fixture(Actions::default());
    let error = fixture.preparer.prepare(journaled(&[])).await.unwrap_err();
    assert!(error.contains("coordinator and worker open sessions changed"), "{error}");
    assert!(fixture.actions.applied.lock().unwrap().is_empty());
    assert_eq!(fixture.boundary.released.load(Ordering::SeqCst), 1);
    assert!(!fixture.harness.manager.keeper_update_prepared(), "admission reopened");
    assert_eq!(input(&fixture, 1).await, WorkerInputResult::Accepted { written_bytes: 1 });
}

#[tokio::test]
async fn a_failed_update_action_reopens_admission_and_releases_the_boundary() {
    let fixture = fixture(Actions {
        apply_fails: true,
        ..Actions::default()
    });
    let error = fixture.preparer.prepare(journaled(&[SESSION])).await.unwrap_err();
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
    let answer = fixture.preparer.prepare(maintenance(true, &[SESSION])).await.unwrap();
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
    assert_eq!(fixture.boundary.acquired.load(Ordering::SeqCst), 1, "the second waits its turn");
    answer.send(Ok("shutdown")).unwrap();
    assert!(first.await.unwrap().is_ok());
    assert!(second.await.unwrap().is_ok());
    assert_eq!(fixture.boundary.acquired.load(Ordering::SeqCst), 2);
    assert!(!fixture.harness.manager.keeper_update_prepared(), "both preparations released admission");
}
