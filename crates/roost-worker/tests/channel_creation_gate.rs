//! Ports v2 `apps/worker/tests/session/session-channel-creation-gate.test.ts`:
//! keeper-update preparation drains a spawn (and a respawn) that already held
//! its lease, refuses a new creation at once, stays closed after a successful
//! preparation, and reopens only on rollback. The keeper spawn is held on a
//! channel, as v2 deferred `pool.spawn`. The update action itself (v2
//! `createKeeperUpdatePrepareHandler`) belongs to the keeper-update owner.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "session_support/mod.rs"]
mod session_support;

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::ChannelId;
use roost_worker::browser_commands::session_lifecycle::SessionOutcome;
use roost_worker::session::channel_creation_gate::{CHANNEL_CREATION_REFUSAL, ChannelCreationGate};
use roost_worker::session::control_lanes::ControlLanes;
use roost_worker::session::keeper_admission::{Admission, AdmissionKind};
use roost_worker::session::sinks::ChannelBinding;
use roost_worker::session::spawn::ShellSpawner;
use roost_worker::shell_spec::ShellSpec;
use session_support::{Harness, SESSION, channel, session_id};

/// What the keeper does with the next spawn it is asked for.
enum Spawn {
    /// Answer at once with this child pid.
    Now(u32),
    /// Block until the test sends the answer (v2 `deferKeeperSpawn`).
    Held(mpsc::Receiver<Result<u32, String>>),
}

/// A keeper whose spawns are scripted, and which says when one is entered.
struct ScriptedSpawner {
    script: Mutex<VecDeque<Spawn>>,
    entered: Mutex<mpsc::Sender<()>>,
}

impl ShellSpawner for ScriptedSpawner {
    fn spawn_channel(
        &self,
        _channel_id: ChannelId,
        _spec: &ShellSpec,
        _cols: u16,
        _rows: u16,
        _binding: Arc<dyn ChannelBinding>,
    ) -> Result<u32, String> {
        let next = self.script.lock().unwrap().pop_front();
        let _ = self.entered.lock().unwrap().send(());
        match next {
            Some(Spawn::Now(pid)) => Ok(pid),
            Some(Spawn::Held(answer)) => answer.recv().map_err(|_| "test cleanup".to_string())?,
            None => Err("the script ran out of spawns".to_string()),
        }
    }
    fn kill_channel(&self, _channel_id: ChannelId) {}
}

struct Fixture {
    harness: Harness,
    entered: Arc<Mutex<mpsc::Receiver<()>>>,
    answer: mpsc::Sender<Result<u32, String>>,
    spawner: Arc<ScriptedSpawner>,
}

/// A manager whose FIRST keeper spawn is held until `answer` fires.
fn holding_first_spawn() -> Fixture {
    let (answer, held) = mpsc::channel();
    let (entered_tx, entered) = mpsc::channel();
    let spawner = Arc::new(ScriptedSpawner {
        script: Mutex::new(VecDeque::from([Spawn::Held(held)])),
        entered: Mutex::new(entered_tx),
    });
    Fixture {
        harness: Harness::with_spawner(Arc::clone(&spawner) as Arc<dyn ShellSpawner>),
        entered: Arc::new(Mutex::new(entered)),
        answer,
        spawner,
    }
}

async fn entered(fixture: &Fixture) {
    let entered = Arc::clone(&fixture.entered);
    tokio::task::spawn_blocking(move || entered.lock().unwrap().recv().unwrap())
        .await
        .unwrap();
}

fn child_pid(harness: &Harness) -> Option<u32> {
    harness
        .table
        .with_record(&session_id(SESSION), |record| record.child_pid)
        .flatten()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preparation_drains_an_admitted_spawn_rejects_a_new_spawn_and_stays_closed_on_success() {
    let fixture = holding_first_spawn();
    let manager = Arc::clone(&fixture.harness.manager);
    let admitted = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move {
            manager
                .open_shell("/tmp".into(), Some(80), Some(24), Some(session_id(SESSION)))
                .await
        }
    });
    entered(&fixture).await;
    let preparation = tokio::spawn(manager.begin_keeper_update_preparation());
    assert!(
        manager.keeper_update_prepared(),
        "preparation must close admission synchronously"
    );

    let refused = manager.open_shell("/tmp".into(), None, None, None).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|refusal| refusal.to_string().contains(CHANNEL_CREATION_REFUSAL)),
        "a new spawn during preparation was not refused: {refused:?}"
    );
    tokio::task::yield_now().await;
    assert!(
        !preparation.is_finished(),
        "preparation finished while a leased spawn was in flight"
    );

    fixture.answer.send(Ok(4321)).unwrap();
    assert!(matches!(
        admitted.await.unwrap(),
        Ok(SessionOutcome::Spawned { .. })
    ));
    assert_eq!(child_pid(&fixture.harness), Some(4321));
    let _rollback = preparation.await.unwrap();

    let still_closed = manager.open_shell("/tmp".into(), None, None, None).await;
    assert!(
        still_closed.is_err_and(|refusal| refusal.to_string().contains(CHANNEL_CREATION_REFUSAL)),
        "a successful preparation reopened channel creation"
    );
    let write = manager
        .control_lanes()
        .admit(channel(1), AdmissionKind::TerminalInput);
    assert!(
        matches!(write, Admission::Refused(_)),
        "a terminal write reached a keeper about to be replaced"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_admitted_respawn_drains_before_preparation_and_rollback_reopens_admission() {
    let fixture = holding_first_spawn();
    let manager = Arc::clone(&fixture.harness.manager);
    let admitted = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move {
            manager
                .respawn_lost_child(&session_id(SESSION), "/tmp", 80, 24)
                .await
        }
    });
    entered(&fixture).await;
    let preparation = tokio::spawn(manager.begin_keeper_update_preparation());
    tokio::task::yield_now().await;
    assert!(!preparation.is_finished());

    fixture
        .answer
        .send(Err("injected keeper spawn failure".to_string()))
        .unwrap();
    let failed = admitted.await.unwrap();
    assert!(failed.is_err_and(|refusal| {
        refusal
            .to_string()
            .contains("injected keeper spawn failure")
    }));
    let rollback = preparation.await.unwrap();
    rollback.rollback();
    assert!(!manager.keeper_update_prepared());

    fixture
        .spawner
        .script
        .lock()
        .unwrap()
        .push_back(Spawn::Now(8765));
    let reopened = manager
        .respawn_lost_child(&session_id(SESSION), "/tmp", 80, 24)
        .await;
    // v2 asserts only the reopened respawn's child pid (`respawnIfMissing`
    // answers with the live record, `SessionOutcome::Live` here).
    assert!(
        matches!(reopened, Ok(SessionOutcome::Live { .. })),
        "rollback did not reopen creation: {reopened:?}"
    );
    assert_eq!(child_pid(&fixture.harness), Some(8765));
}

/// v2 `beginPreparation`'s rollback releases only ITS attempt, once: an
/// overlapping preparation keeps admission closed, and a repeated rollback of
/// the same attempt cannot release the other one.
#[tokio::test]
async fn a_rollback_releases_only_its_own_preparation_once() {
    let lanes = Arc::new(ControlLanes::new());
    let gate = ChannelCreationGate::new(Arc::clone(&lanes));
    let first = gate.begin_preparation().await;
    let second = gate.begin_preparation().await;
    first.rollback();
    first.rollback();
    assert!(
        gate.preparation_active(),
        "a repeated rollback released another preparation"
    );
    assert!(gate.try_acquire().is_none());
    assert!(lanes.keeper_update_prepared());
    second.rollback();
    assert!(!gate.preparation_active());
    assert!(!lanes.keeper_update_prepared());
    assert!(gate.try_acquire().is_some());
}
