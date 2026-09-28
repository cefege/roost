//! A closed tab's PTY is killed once its undo window runs out (v2
//! `closeSession.ts` `killAfterUndo`): a graceful kill at the deadline, a
//! forced one if the graceful kill is refused, and a `Close failed:` card when
//! the call itself fails.
//!
//! Mutation proof: removing the `issue_due_kills` call from `handle_sweep`
//! fails `the_kill_is_issued_at_the_window_deadline_and_not_before`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::rpc::unary::CallError;
use roost_client_core::event::ClientEvent;
use roost_client_core::store::pending_close::{
    CloseLabels, UNDO_WINDOW_MS, schedule_close, undo_one,
};
use roost_client_core::store::toasts::ToastKind;
use roost_client_core::{ClientCore, Effect, RpcCall, RpcResult};

const SESSION: &str = "00000000-0000-4000-8000-00000000000a";

/// Every `SessionsKill` among `effects`, as (call id, session, force).
fn kills(effects: &[Effect]) -> Vec<(u64, String, bool)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Rpc(RpcCall::SessionsKill {
                call_id,
                session_id,
                force,
            }) => Some((*call_id, session_id.clone(), *force)),
            _ => None,
        })
        .collect()
}

fn sweep(core: &mut ClientCore, now_ms: u64) -> Vec<(u64, String, bool)> {
    kills(&core.handle(ClientEvent::Sweep { now_ms }))
}

/// A client with `SESSION` closed at t=1000, swept to its deadline; returns the
/// graceful kill's call id.
fn closed_and_swept() -> (ClientCore, u64) {
    let mut core = ClientCore::in_memory("tab-close");
    schedule_close(core.store_mut(), SESSION, CloseLabels::default(), 1_000);
    let issued = sweep(&mut core, 1_000 + UNDO_WINDOW_MS);
    assert_eq!(issued.len(), 1);
    (core, issued[0].0)
}

fn answer(core: &mut ClientCore, call_id: u64, force: bool, accepted: bool) -> Vec<Effect> {
    core.handle(ClientEvent::RpcResultReceived(
        RpcResult::SessionKillAnswered {
            call_id,
            session_id: SESSION.to_owned(),
            force,
            accepted,
        },
    ))
}

#[test]
fn the_kill_is_issued_at_the_window_deadline_and_not_before() {
    let mut core = ClientCore::in_memory("tab-close");
    schedule_close(core.store_mut(), SESSION, CloseLabels::default(), 1_000);
    assert!(sweep(&mut core, 1_000 + UNDO_WINDOW_MS - 1).is_empty());
    let issued = sweep(&mut core, 1_000 + UNDO_WINDOW_MS);
    assert_eq!(issued.len(), 1);
    assert_eq!((issued[0].1.as_str(), issued[0].2), (SESSION, false));
    assert!(
        sweep(&mut core, 1_000 + 2 * UNDO_WINDOW_MS).is_empty(),
        "one close is one kill"
    );
}

#[test]
fn an_undone_close_is_never_killed() {
    let mut core = ClientCore::in_memory("tab-close");
    schedule_close(core.store_mut(), SESSION, CloseLabels::default(), 1_000);
    assert!(undo_one(core.store_mut(), SESSION).is_some());
    assert!(sweep(&mut core, 1_000 + UNDO_WINDOW_MS).is_empty());
}

#[test]
fn a_refused_graceful_kill_is_forced_once() {
    let (mut core, call_id) = closed_and_swept();
    assert!(kills(&answer(&mut core, call_id, false, true)).is_empty());

    let (mut core, call_id) = closed_and_swept();
    let forced = kills(&answer(&mut core, call_id, false, false));
    assert_eq!(forced.len(), 1);
    assert_eq!((forced[0].1.as_str(), forced[0].2), (SESSION, true));
    assert!(
        kills(&answer(&mut core, forced[0].0, true, false)).is_empty(),
        "a refused forced kill is not retried"
    );
}

#[test]
fn a_failed_kill_raises_a_close_failed_card() {
    let (mut core, call_id) = closed_and_swept();
    let effects = core.handle(ClientEvent::RpcResultReceived(RpcResult::Failed {
        call_id,
        error: CallError::Network("offline".to_owned()),
    }));
    assert!(kills(&effects).is_empty());
    let cards: Vec<_> = core.store().toasts.toasts().collect();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].kind, ToastKind::Err);
    assert_eq!(cards[0].msg, "Close failed: network: offline");
}
