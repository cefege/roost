//! A failed adoption's teardown, ported from v2
//! `apps/worker/tests/session/session-resume-orphan-teardown.test.ts`: when the
//! adoption fails AFTER the survivor was reattached, the survivor is killed
//! through the keeper's own kill, its channel is marked recently closed for
//! the tail gate, the coordinator gets the standard closed tombstone, and the
//! orphan's trailing output is neither parsed nor counted as a degraded keeper.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use roost_protocol::wire::event::SessionEvent;
use roost_worker::session::resume::AdoptRefusal;

use session_support::{Harness, NOW, SESSION, ScriptedKeeper, session_id};

const CHANNEL: u16 = 23;

/// Adopt against a keeper whose applied geometry is `cols`x`rows`; 0x0 is the
/// "keeper did not report valid terminal geometry" failure v2 forces with a
/// null terminal state. The degraded hook counts its calls.
async fn adopt(cols: u16, rows: u16) -> (Harness, Arc<AtomicUsize>, bool) {
    let keeper = Arc::new(ScriptedKeeper::with_survivor(CHANNEL, 55_332));
    keeper.applied.lock().expect("held").cols = cols;
    keeper.applied.lock().expect("held").rows = rows;
    let harness = Harness::with_keeper(keeper);
    let degraded = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&degraded);
    harness.manager.keeper_health().set_hook(Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    }));
    let adopted = harness
        .manager
        .adopt_survivor(&harness.adoption(SESSION, CHANNEL, "/"))
        .await;
    let ok = match &adopted {
        Ok(_) => true,
        Err(failure) => {
            assert!(
                matches!(failure.refusal, AdoptRefusal::Unreplayable { .. }),
                "{failure}"
            );
            assert!(
                failure.abandoned,
                "a post-reattach failure kills the survivor"
            );
            false
        }
    };
    (harness, degraded, ok)
}

#[tokio::test]
async fn a_forced_post_reattach_rejection_kills_the_adopted_keeper_channel() {
    let (harness, _, adopted) = adopt(0, 0).await;
    assert!(!adopted);
    assert_eq!(harness.keeper.killed(), vec![CHANNEL]);
}

#[tokio::test]
async fn the_killed_channel_is_registered_recently_closed_for_the_tail_gate() {
    let (harness, _, _) = adopt(0, 0).await;
    assert!(
        harness
            .manager
            .keeper_health()
            .is_recently_closed(CHANNEL, NOW)
    );
}

#[tokio::test]
async fn coord_receives_the_standard_closed_tombstone_for_the_unadoptable_row() {
    let (harness, _, _) = adopt(0, 0).await;
    let tombstones: Vec<SessionEvent> = harness
        .sink
        .published()
        .into_iter()
        .filter(|event| matches!(event, SessionEvent::Closed { session_id, exit_code: None, .. } if *session_id == session_id_of()))
        .collect();
    assert_eq!(
        tombstones.len(),
        1,
        "exactly one closed with no exit code: the kill was ours"
    );
}

#[tokio::test]
async fn orphan_output_afterwards_is_neither_handled_nor_counted_toward_degradation() {
    let (harness, degraded, _) = adopt(0, 0).await;
    assert!(harness.table.channel_of(&session_id_of()).is_none());
    let binding = harness.keeper.delivered();
    for index in 0..10 {
        binding.on_output(format!("orphan-{index}").as_bytes());
    }
    assert!(
        harness.table.channel_of(&session_id_of()).is_none(),
        "still record-less"
    );
    assert!(
        harness.delivery.parsed.lock().expect("held").is_empty(),
        "nothing was parsed"
    );
    assert_eq!(
        degraded.load(Ordering::SeqCst),
        0,
        "a tail inside the gate is not a degraded keeper"
    );
}

#[tokio::test]
async fn a_successful_adoption_still_writes_no_kill_and_emits_no_tombstone() {
    let (harness, _, adopted) = adopt(80, 24).await;
    assert!(adopted);
    assert!(harness.table.channel_of(&session_id_of()).is_some());
    assert!(harness.keeper.killed().is_empty());
    assert_eq!(harness.sink.closed_events(), 0);
    assert!(
        !harness
            .manager
            .keeper_health()
            .is_recently_closed(CHANNEL, NOW)
    );
}

fn session_id_of() -> roost_protocol::wire::brand::SessionId {
    session_id(SESSION)
}
