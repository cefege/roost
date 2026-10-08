#![cfg(unix)]
//! Keeper-update preparation against a keeper spawn already in flight: the
//! preparer waits the admitted creation out before it touches the reconcile
//! boundary or the update action. Ports tests 1 and 2 of v2
//! `apps/worker/tests/session/session-channel-creation-gate.test.ts` at the
//! handler seam (`transport/coord-link-keeper-update.ts`); the gate-only halves
//! are `channel_creation_gate.rs`'s.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "keeper_update_prepare_support/mod.rs"]
mod keeper_update_prepare_support;
#[path = "keeper_update_support/mod.rs"]
mod keeper_update_support;
#[path = "session_support/mod.rs"]
mod session_support;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use keeper_update_prepare_support::{Actions, holding_spawn, journaled, let_it_run};
use roost_worker::link_ports::KeeperUpdatePort;
use roost_worker::session::channel_creation_gate::CHANNEL_CREATION_REFUSAL;
use session_support::{SESSION, session_id};

/// v2 session-channel-creation-gate test 1: an admitted spawn is waited out
/// before the reconcile boundary or the action is touched, a new spawn is
/// refused at once, and success leaves creation closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preparation_waits_out_an_admitted_spawn_before_the_boundary_and_the_action() {
    let (fixture, answer, entered) = holding_spawn(Actions::default());
    let manager = Arc::clone(&fixture.harness.manager);
    let admitted = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move {
            manager
                .open_shell("/tmp".into(), Some(80), Some(24), Some(session_id(SESSION)))
                .await
        }
    });
    tokio::task::spawn_blocking(move || entered.recv().unwrap())
        .await
        .unwrap();
    let preparation = tokio::spawn(fixture.preparer.prepare(journaled(&[SESSION])));
    let refused = manager.open_shell("/tmp".into(), None, None, None).await;
    assert!(refused.is_err_and(|refusal| refusal.to_string().contains(CHANNEL_CREATION_REFUSAL)));
    let_it_run().await;
    assert_eq!(
        fixture.boundary.acquired.load(Ordering::SeqCst),
        0,
        "the boundary waited for the spawn"
    );
    assert!(
        fixture.actions.applied.lock().unwrap().is_empty(),
        "the action waited for the spawn"
    );

    answer.send(Ok(4321)).unwrap();
    assert!(admitted.await.unwrap().is_ok());
    let answer = preparation.await.unwrap().unwrap();
    assert_eq!(answer["outcome"], "preserved");
    let live_channels: Vec<u32> = fixture
        .harness
        .table
        .live()
        .iter()
        .map(|(_, channel)| u32::from(*channel))
        .collect();
    let applied = fixture.actions.applied.lock().unwrap().clone();
    assert_eq!(applied.len(), 1);
    assert_eq!(
        applied[0].coordinator_open_session_ids,
        vec![SESSION.to_owned()]
    );
    assert_eq!(applied[0].worker_open_channel_ids, live_channels);
    assert_eq!(fixture.boundary.acquired.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.boundary.released.load(Ordering::SeqCst), 0);
    let still_closed = manager.open_shell("/tmp".into(), None, None, None).await;
    assert!(
        still_closed.is_err_and(|refusal| refusal.to_string().contains(CHANNEL_CREATION_REFUSAL))
    );
}

/// v2 session-channel-creation-gate test 2: a failed admitted respawn drains
/// first, then the failed action releases the boundary and reopens admission.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_admitted_respawn_drains_then_the_failed_action_releases_everything() {
    let (fixture, answer, entered) = holding_spawn(Actions {
        apply_fails: true,
        ..Actions::default()
    });
    let manager = Arc::clone(&fixture.harness.manager);
    let admitted = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move {
            manager
                .respawn_lost_child(&session_id(SESSION), "/tmp", 80, 24)
                .await
        }
    });
    tokio::task::spawn_blocking(move || entered.recv().unwrap())
        .await
        .unwrap();
    let preparation = tokio::spawn(fixture.preparer.prepare(journaled(&[])));
    let_it_run().await;
    assert_eq!(fixture.boundary.acquired.load(Ordering::SeqCst), 0);
    assert!(fixture.actions.applied.lock().unwrap().is_empty());

    answer
        .send(Err("injected keeper spawn failure".to_owned()))
        .unwrap();
    assert!(admitted.await.unwrap().is_err());
    let error = preparation.await.unwrap().unwrap_err();
    assert_eq!(error, "injected keeper update failure");
    assert_eq!(fixture.actions.applied.lock().unwrap().len(), 1);
    assert_eq!(fixture.boundary.acquired.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.boundary.released.load(Ordering::SeqCst), 1);
    assert!(
        !manager.keeper_update_prepared(),
        "the failed preparation reopened admission"
    );
}
