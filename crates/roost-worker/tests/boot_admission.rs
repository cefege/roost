//! Boot admission while the link already dials: the boot reconcile reads the
//! coordinator only once the durable session-event replay has drained, and the
//! snapshot is activated only after that pass settled. Ports v2
//! `apps/worker/src/boot/worker-boot-admission.ts` and `boot-reconcile.ts:69-80`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use roost_observability::clock::EventClock;
use roost_worker::runtime::boot_admission::complete_worker_boot_admission;
use roost_worker::runtime::boot_order::{BootSequence, StepId};
use roost_worker::runtime::heartbeat::KeeperReconciliation;
use roost_worker::runtime::reconcile_gate::{
    KeeperRemediation, PassAdmission, ReconcileGate, ReconcileOutcome, ReconcilePass,
};
use roost_worker::runtime::session_reconcile::{ReconcileFailure, ReconcileSummary};
use roost_worker::runtime::snapshot_source::SnapshotActivation;
use roost_worker::runtime::stop::StopRequests;
use roost_worker::session::durable_delivery::DurableDelivery;
use roost_worker::uplink::OwnerFuture;
use tokio::sync::Notify;

/// Long enough for a pass that was free to start to have read.
const SETTLE: Duration = Duration::from_millis(200);
const PATIENCE: Duration = Duration::from_secs(10);

/// A pass whose run is its read of the coordinator: it counts reads, can be
/// held mid-pass, and can refuse.
#[derive(Default)]
struct RecordingPass {
    reads: AtomicUsize,
    hold: AtomicBool,
    release: Notify,
    refuse: AtomicBool,
}

impl ReconcilePass for RecordingPass {
    fn run(self: Arc<Self>, _reason: &'static str) -> OwnerFuture<ReconcileOutcome> {
        Box::pin(async move {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.hold.load(Ordering::SeqCst) {
                self.release.notified().await;
            }
            if self.refuse.load(Ordering::SeqCst) {
                return Err(ReconcileFailure::recoverable("sessionsList timed out"));
            }
            Ok(ReconcileSummary::default())
        })
    }
}

struct NoRemediation;

impl KeeperRemediation for NoRemediation {
    fn keeper_update_prepared(&self) -> bool {
        false
    }
    fn restart_keeper(&self) -> bool {
        false
    }
}

#[derive(Debug)]
struct FixedClock;

impl EventClock for FixedClock {
    fn now_epoch_ms(&self) -> i64 {
        1_700_000_000_000
    }
    fn mono_ns(&self) -> u64 {
        0
    }
}

struct Rig {
    gate: ReconcileGate,
    pass: Arc<RecordingPass>,
    replay: Arc<DurableDelivery>,
    snapshot: SnapshotActivation,
    _stop: StopRequests,
}

fn rig() -> Rig {
    let pass = Arc::new(RecordingPass::default());
    let replay = Arc::new(DurableDelivery::new());
    let (stop, _signal) = StopRequests::channel();
    let gate = ReconcileGate::new(
        Arc::clone(&pass) as Arc<dyn ReconcilePass>,
        PassAdmission {
            durable_replay: Arc::clone(&replay),
        },
        Arc::new(NoRemediation),
        Arc::new(FixedClock),
        KeeperReconciliation::default(),
        stop.clone(),
        tokio::runtime::Handle::current(),
    );
    Rig {
        gate,
        pass,
        replay,
        snapshot: SnapshotActivation::held(Arc::new(Notify::new())),
        _stop: stop,
    }
}

/// The boot admission as `boot_sequence` runs it, with the link step done.
fn admit(rig: &Rig) -> tokio::task::JoinHandle<Result<bool, String>> {
    let (gate, snapshot) = (rig.gate.clone(), rig.snapshot.clone());
    tokio::spawn(async move {
        let mut sequence = BootSequence::new();
        for step in [
            StepId::Identity,
            StepId::KeeperAdmission,
            StepId::CoordinatorLink,
        ] {
            sequence.complete(step).unwrap();
        }
        complete_worker_boot_admission(&gate, &snapshot, &mut sequence)
            .await
            .map(|(_, readiness)| readiness.is_ready())
            .map_err(|refusal| refusal.to_string())
    })
}

async fn until_read(pass: &RecordingPass) {
    tokio::time::timeout(PATIENCE, async {
        while pass.reads.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the pass never read the coordinator");
}

/// v2 `beforeRecoveryRead` then `activateSnapshotProvider`: no read while the
/// replay is pending, no snapshot while the pass runs.
#[tokio::test]
async fn the_boot_pass_reads_only_after_the_replay_and_the_snapshot_only_follows_the_pass() {
    let rig = rig();
    rig.pass.hold.store(true, Ordering::SeqCst);
    let admission = admit(&rig);
    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        rig.pass.reads.load(Ordering::SeqCst),
        0,
        "the boot pass read the coordinator before the durable replay drained"
    );

    rig.replay.mark_drained();
    until_read(&rig.pass).await;
    tokio::time::sleep(SETTLE).await;
    assert!(
        !rig.snapshot.is_active(),
        "the snapshot was activated mid-pass"
    );

    rig.pass.release.notify_one();
    let ready = tokio::time::timeout(PATIENCE, admission)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready, Ok(true));
    assert!(rig.snapshot.is_active());
}

/// v2 `completeWorkerBootAdmission` throws before activation: a refused boot
/// pass never lets the snapshot publish.
#[tokio::test]
async fn a_refused_boot_pass_leaves_the_snapshot_held() {
    let rig = rig();
    rig.pass.refuse.store(true, Ordering::SeqCst);
    rig.replay.mark_drained();
    let refused = tokio::time::timeout(PATIENCE, admit(&rig))
        .await
        .unwrap()
        .unwrap();
    assert!(refused.unwrap_err().contains("sessionsList timed out"));
    assert!(!rig.snapshot.is_active());
}

/// v2 `dispose` rejects the waiter: a link that stopped for good refuses the
/// boot pass it was holding instead of leaving boot waiting for ever.
#[tokio::test]
async fn a_disposed_link_refuses_the_boot_pass_without_a_read() {
    let rig = rig();
    let admission = admit(&rig);
    tokio::time::sleep(SETTLE).await;
    rig.replay.dispose();
    let refused = tokio::time::timeout(PATIENCE, admission)
        .await
        .unwrap()
        .unwrap();
    assert!(
        refused
            .unwrap_err()
            .contains("coordinator link is disposed")
    );
    assert_eq!(rig.pass.reads.load(Ordering::SeqCst), 0);
    assert!(!rig.snapshot.is_active());
}
