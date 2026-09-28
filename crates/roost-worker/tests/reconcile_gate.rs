//! The reconcile gate, ported from v2 `apps/worker/tests/boot/boot-reconcile*.test.ts`
//! and `keeper-death-reconcile.test.ts`: callers join the pass in flight, a
//! prepared keeper update blocks new passes and waits out the current one, a
//! keeper death drives a pass unless an update is prepared, a durability loss
//! stops the worker, and a degraded keeper is restarted only past the grace
//! window and within the restart budget. Drives `runtime::reconcile_gate` over
//! a scripted pass and remediation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

use roost_observability::clock::EventClock;
use roost_worker::agents::reference_admission::AgentReferenceAdmissionGate;
use roost_worker::keeper_pool::KeeperUpdateBoundary;
use roost_worker::runtime::heartbeat::KeeperReconciliation;
use roost_worker::runtime::reconcile_gate::{
    KeeperRemediation, PassAdmission, RECONCILE_BLOCKED_BY_UPDATE, ReconcileGate, ReconcileOutcome,
    ReconcilePass,
};
use roost_worker::runtime::session_reconcile::{ReconcileFailure, ReconcileSummary};
use roost_worker::runtime::stop::StopRequests;
use roost_worker::session::durable_delivery::DurableDelivery;
use roost_worker::uplink::OwnerFuture;
use tokio::sync::Notify;

#[derive(Debug, Default)]
struct ManualClock(AtomicI64);

impl EventClock for ManualClock {
    fn now_epoch_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn mono_ns(&self) -> u64 {
        0
    }
}

/// A pass that counts its runs, optionally waits for a release, and answers
/// with `fatal` when asked to.
#[derive(Default)]
struct ScriptedPass {
    runs: AtomicUsize,
    hold: AtomicBool,
    release: Notify,
    fatal: AtomicBool,
}

impl ReconcilePass for ScriptedPass {
    fn run(self: Arc<Self>, _reason: &'static str) -> OwnerFuture<ReconcileOutcome> {
        Box::pin(async move {
            self.runs.fetch_add(1, Ordering::SeqCst);
            if self.hold.load(Ordering::SeqCst) {
                self.release.notified().await;
            }
            if self.fatal.load(Ordering::SeqCst) {
                return Err(ReconcileFailure {
                    reason: "durability".to_owned(),
                    fatal: true,
                });
            }
            Ok(ReconcileSummary::default())
        })
    }
}

#[derive(Default)]
struct ScriptedRemediation {
    prepared: AtomicBool,
    restarts: AtomicUsize,
}

impl KeeperRemediation for ScriptedRemediation {
    fn keeper_update_prepared(&self) -> bool {
        self.prepared.load(Ordering::SeqCst)
    }
    fn restart_keeper(&self) -> bool {
        self.restarts.fetch_add(1, Ordering::SeqCst);
        true
    }
}

struct Rig {
    gate: ReconcileGate,
    pass: Arc<ScriptedPass>,
    remediation: Arc<ScriptedRemediation>,
    clock: Arc<ManualClock>,
    stop: StopRequests,
}

fn rig() -> Rig {
    let pass = Arc::new(ScriptedPass::default());
    let remediation = Arc::new(ScriptedRemediation::default());
    let clock = Arc::new(ManualClock::default());
    let (stop, _signal) = StopRequests::channel();
    // A link with nothing left to replay: these passes never wait on it.
    let durable_replay = Arc::new(DurableDelivery::new());
    durable_replay.mark_drained();
    let admission = PassAdmission {
        reference_admission: AgentReferenceAdmissionGate::new(),
        durable_replay,
    };
    let gate = ReconcileGate::new(
        Arc::clone(&pass) as Arc<dyn ReconcilePass>,
        admission,
        Arc::clone(&remediation) as Arc<dyn KeeperRemediation>,
        Arc::clone(&clock) as Arc<dyn EventClock>,
        KeeperReconciliation::default(),
        stop.clone(),
        tokio::runtime::Handle::current(),
    );
    Rig {
        gate,
        pass,
        remediation,
        clock,
        stop,
    }
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(20)).await;
}

#[tokio::test]
async fn concurrent_callers_join_the_pass_in_flight() {
    let rig = rig();
    rig.pass.hold.store(true, Ordering::SeqCst);
    let first = rig.gate.reconcile_open_sessions("boot");
    let second = rig.gate.reconcile_open_sessions("keeper_death");
    settle().await;
    rig.pass.release.notify_waiters();
    assert!(first.await.is_ok() && second.await.is_ok());
    assert_eq!(
        rig.pass.runs.load(Ordering::SeqCst),
        1,
        "the second caller joined the first pass"
    );
}

#[tokio::test]
async fn a_prepared_keeper_update_waits_out_the_pass_and_blocks_new_ones() {
    let rig = rig();
    rig.pass.hold.store(true, Ordering::SeqCst);
    let running = rig.gate.reconcile_open_sessions("boot");
    settle().await;
    let acquire = tokio::spawn({
        let gate = rig.gate.clone();
        async move { gate.acquire().await }
    });
    settle().await;
    assert!(
        !acquire.is_finished(),
        "the boundary waits for the pass in flight"
    );
    rig.pass.release.notify_waiters();
    running.await.expect("the pass completes");
    let release = acquire.await.unwrap().expect("the boundary is held");

    let refused = rig.gate.reconcile_open_sessions("keeper_death").await;
    assert_eq!(refused.unwrap_err().reason, RECONCILE_BLOCKED_BY_UPDATE);
    release();
    rig.pass.hold.store(false, Ordering::SeqCst);
    assert!(
        rig.gate.reconcile_open_sessions("boot").await.is_ok(),
        "the release reopens the gate"
    );
}

#[tokio::test]
async fn a_keeper_death_runs_a_pass_unless_an_update_is_prepared() {
    let rig = rig();
    rig.remediation.prepared.store(true, Ordering::SeqCst);
    rig.gate.on_keeper_death();
    settle().await;
    assert_eq!(
        rig.pass.runs.load(Ordering::SeqCst),
        0,
        "suppressed while an update is prepared"
    );
    rig.remediation.prepared.store(false, Ordering::SeqCst);
    rig.gate.on_keeper_death();
    settle().await;
    assert_eq!(rig.pass.runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_lost_durability_stops_the_worker() {
    let rig = rig();
    rig.pass.fatal.store(true, Ordering::SeqCst);
    let failure = rig.gate.reconcile_open_sessions("boot").await.unwrap_err();
    assert!(failure.fatal);
    assert!(
        rig.stop.is_requested(),
        "v2 rethrows a durability error to the uncaught handler"
    );
}

#[tokio::test]
async fn a_degraded_keeper_restarts_only_past_the_grace_window_and_within_budget() {
    let rig = rig();
    rig.clock.0.store(1_000_000, Ordering::SeqCst);
    rig.gate
        .reconcile_open_sessions("boot")
        .await
        .expect("admitted");
    settle().await;

    rig.clock.0.store(1_000_000 + 1_000, Ordering::SeqCst);
    rig.gate.on_keeper_degraded();
    assert_eq!(
        rig.remediation.restarts.load(Ordering::SeqCst),
        0,
        "inside the 90 s grace window"
    );

    for (offset, expected) in [(100_000, 1), (100_001, 2), (100_002, 2)] {
        rig.clock.0.store(1_000_000 + offset, Ordering::SeqCst);
        rig.gate.on_keeper_degraded();
        assert_eq!(
            rig.remediation.restarts.load(Ordering::SeqCst),
            expected,
            "at +{offset} ms"
        );
    }
}
