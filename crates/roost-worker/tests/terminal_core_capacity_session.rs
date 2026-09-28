//! The session layer's core leases, through the real `SessionManager`: an
//! adopted core is resident until its record is torn down, an over-cap
//! adoption touches nothing and returns its close claim, and an abandoned
//! adoption gives its slot back. Ports the record-teardown and resume cases of
//! `apps/worker/tests/terminal/terminal-core-capacity.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use std::sync::Arc;

use roost_worker::session::binding::RESUME_STAGE_CAP_BYTES;
use roost_worker::session::resume::AdoptRefusal;
use roost_worker::terminal_core_capacity::{
    TerminalCoreAllocationKind, TerminalCoreCapacityRefusalReason,
};

use session_support::{Harness, SESSION, ScriptedKeeper};

fn used_pending(harness: &Harness) -> (u32, u32) {
    let snapshot = harness.core_capacity.snapshot();
    (snapshot.used, snapshot.pending)
}

#[tokio::test]
async fn an_adopted_core_holds_its_lease_until_the_session_closes() {
    let keeper = Arc::new(ScriptedKeeper::with_survivor(7, 4242));
    let harness = Harness::with_capacity(Arc::clone(&keeper), Some(1));
    harness
        .manager
        .adopt_survivor(&harness.adoption(SESSION, 7, "/home/user/project"))
        .await
        .expect("the survivor is adoptable");
    assert_eq!(
        used_pending(&harness),
        (1, 0),
        "the adopted record's core is resident"
    );

    harness
        .manager
        .close_channel(7, Some(0))
        .await
        .expect("the adopted session closes");
    assert_eq!(
        used_pending(&harness),
        (0, 0),
        "record teardown kept the core's slot"
    );
    assert!(
        harness
            .core_capacity
            .reserve(TerminalCoreAllocationKind::Fresh)
            .is_ok(),
        "the freed slot is admissible again"
    );
}

#[tokio::test]
async fn an_over_cap_adoption_touches_nothing_and_returns_its_close_claim() {
    let keeper = Arc::new(ScriptedKeeper::with_survivor(7, 4242));
    let harness = Harness::with_capacity(Arc::clone(&keeper), Some(0));
    let request = harness.adoption(SESSION, 7, "/home/user/project");
    assert_eq!(harness.sink.store.lock().unwrap().live_reservations(), 1);

    let refused = harness
        .manager
        .adopt_survivor(&request)
        .await
        .expect_err("a zero-capacity worker cannot adopt a core");
    match refused.refusal {
        AdoptRefusal::TerminalCoreCapacity { channel, refusal } => {
            assert_eq!(channel, 7);
            assert_eq!(
                refusal.reason,
                TerminalCoreCapacityRefusalReason::Allocation(TerminalCoreAllocationKind::Adoption)
            );
        }
        other => panic!("the refusal is not the capacity's: {other:?}"),
    }
    assert!(
        !refused.abandoned,
        "a capacity refusal must not kill the survivor"
    );
    assert!(keeper.killed().is_empty());
    assert!(
        keeper.delivered.lock().unwrap().is_none(),
        "the survivor was rebound"
    );
    assert_eq!(
        harness.sink.store.lock().unwrap().live_reservations(),
        0,
        "the close claim the adoption was handed was kept"
    );
    assert_eq!(used_pending(&harness), (0, 0));
}

#[tokio::test]
async fn an_abandoned_adoption_gives_its_core_slot_back() {
    let keeper = Arc::new(ScriptedKeeper::with_survivor(7, 4242));
    keeper
        .on_rebind
        .lock()
        .unwrap()
        .push(vec![b'x'; RESUME_STAGE_CAP_BYTES + 1]);
    let harness = Harness::with_capacity(Arc::clone(&keeper), Some(1));
    let refused = harness
        .manager
        .adopt_survivor(&harness.adoption(SESSION, 7, "/home/user/project"))
        .await
        .expect_err("a stream with a hole in it is not adopted");
    assert!(matches!(
        refused.refusal,
        AdoptRefusal::StagingOverflow { .. }
    ));
    assert!(refused.abandoned);
    assert_eq!(
        used_pending(&harness),
        (0, 0),
        "the abandoned record kept its core slot"
    );
}
