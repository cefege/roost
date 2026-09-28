// An integration-test module is its own crate and `expect` is denied outside
// `#[cfg(test)]`, so the exemption lives here once.
#![allow(clippy::unwrap_used, clippy::expect_used)]
// Compiled once per test binary, and each binary drives a different subset.
#![allow(dead_code)]

//! Fakes for the keeper-update preparer: v2's injected reconcile boundary and
//! update actions, a keeper spawn the test holds, and the request builders.
//! What calls it: `keeper_update_prepare.rs`, `keeper_update_prepare_drain.rs`
//! (each also declares `session_support` and `keeper_update_support`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use roost_proto::DKeeperUpdatePrepare;
use roost_protocol::keeper_update::KeeperBinding;
use roost_protocol::wire::brand::ChannelId;
use roost_worker::keeper_pool::{
    BoundaryRelease, JournaledKeeperUpdateActionV1, KeeperUpdateActionResult, KeeperUpdateActions,
    KeeperUpdateBoundary, KeeperUpdatePreparer,
};
use roost_worker::session::sinks::ChannelBinding;
use roost_worker::session::spawn::ShellSpawner;
use roost_worker::shell_spec::ShellSpec;
use roost_worker::uplink::OwnerFuture;
use tokio::sync::oneshot;

use crate::session_support::{Harness, SESSION};

pub const CHANNEL: u16 = 7;
pub const ACTIVE: [KeeperBinding; 1] = [KeeperBinding {
    channel_id: 7,
    pid: 5252,
}];

#[derive(Default)]
pub struct Boundary {
    pub acquired: AtomicUsize,
    pub released: Arc<AtomicUsize>,
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
pub struct Actions {
    pub applied: Mutex<Vec<JournaledKeeperUpdateActionV1>>,
    pub apply_fails: bool,
    pub maintenance_calls: AtomicUsize,
    /// When set, the maintenance shutdown waits for this answer.
    pub deferred: Mutex<Option<oneshot::Receiver<Result<&'static str, String>>>>,
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
                keeper_epoch: Some(crate::keeper_update_support::KEEPER_EPOCH.to_owned()),
                binding_digest: Some(crate::keeper_update_support::digest_of(&ACTIVE)),
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

pub struct Fixture {
    pub harness: Harness,
    pub boundary: Arc<Boundary>,
    pub actions: Arc<Actions>,
    pub preparer: Arc<KeeperUpdatePreparer>,
}

pub fn fixture(actions: Actions) -> Fixture {
    let harness = Harness::new();
    harness.install(SESSION, CHANNEL, "/home/user/project", "/home/user/project");
    fixture_over(harness, actions)
}

pub fn fixture_over(harness: Harness, actions: Actions) -> Fixture {
    let boundary = Arc::new(Boundary::default());
    let actions = Arc::new(actions);
    let preparer = Arc::new(KeeperUpdatePreparer::new(
        Arc::clone(&harness.manager),
        Arc::clone(&harness.table),
        Arc::clone(&boundary) as Arc<dyn KeeperUpdateBoundary>,
        Arc::clone(&actions) as Arc<dyn KeeperUpdateActions>,
    ));
    Fixture {
        harness,
        boundary,
        actions,
        preparer,
    }
}

/// A keeper whose one spawn blocks until the test answers it (v2
/// `deferKeeperSpawn`), and which says when that spawn was entered.
pub struct HeldSpawn {
    answer: Mutex<Option<mpsc::Receiver<Result<u32, String>>>>,
    entered: Mutex<mpsc::Sender<()>>,
}

impl ShellSpawner for HeldSpawn {
    fn spawn_channel(
        &self,
        _channel_id: ChannelId,
        _spec: &ShellSpec,
        _cols: u16,
        _rows: u16,
        _binding: Arc<dyn ChannelBinding>,
    ) -> Result<u32, String> {
        let answer = self.answer.lock().unwrap().take();
        let _ = self.entered.lock().unwrap().send(());
        match answer {
            Some(answer) => answer.recv().map_err(|_| "test cleanup".to_owned())?,
            None => Err("the held keeper answers one spawn".to_owned()),
        }
    }
    fn kill_channel(&self, _channel_id: ChannelId) {}
}

/// A preparer over a manager whose first keeper spawn is held.
pub fn holding_spawn(
    actions: Actions,
) -> (
    Fixture,
    mpsc::Sender<Result<u32, String>>,
    mpsc::Receiver<()>,
) {
    let (answer, held) = mpsc::channel();
    let (entered_tx, entered) = mpsc::channel();
    let spawner = Arc::new(HeldSpawn {
        answer: Mutex::new(Some(held)),
        entered: Mutex::new(entered_tx),
    });
    let harness = Harness::with_spawner(spawner as Arc<dyn ShellSpawner>);
    (fixture_over(harness, actions), answer, entered)
}

/// Long enough for a preparation that is NOT waiting to reach its boundary.
pub async fn let_it_run() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

pub fn journaled(sessions: &[&str]) -> DKeeperUpdatePrepare {
    DKeeperUpdatePrepare {
        request_id: "keeper-update-prepare".to_owned(),
        journaled_update_json: Some(
            serde_json::to_string(&crate::keeper_update_support::update(true, &ACTIVE)).unwrap(),
        ),
        direction: "target".to_owned(),
        maintenance: false,
        coordinator_open_session_ids: sessions.iter().map(|id| (*id).to_owned()).collect(),
        force_live: false,
        ..Default::default()
    }
}

pub fn maintenance(force_live: bool, sessions: &[&str]) -> DKeeperUpdatePrepare {
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
