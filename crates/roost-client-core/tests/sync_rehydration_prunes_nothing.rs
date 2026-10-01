//! A re-hydration's snapshot is a FULL REPLACEMENT, so a live session that
//! arrived off the feed while it was in flight must not be the thing it
//! replaces.
//!
//! The failure this pins is the one a redial produces and a first connection
//! cannot: `SessionPlane::apply_snapshot` is the only pruning path in the
//! client, and a snapshot that lands on top of rows already folded off the live
//! feed deletes a session whose terminal is still mounted and painting. The
//! pre-hydration hold exists for exactly that ordering, and it is per LINK —
//! every socket announces fresh domain generations with none of them ready, so
//! every socket re-opens the window in which a snapshot is still to come.
//!
//! The first connection cannot show this: `hydrated` is false from the start,
//! so the hold is already shut when the first snapshot arrives. Only the second
//! socket distinguishes a per-link hold from a one-shot latch, which is why
//! this test redials.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_client_core::effect::{Effect, RpcCall};
use roost_client_core::event::ClientEvent;
use roost_client_core::{ClientCore, SyncDomain, SyncFrame};
use roost_protocol::wire::SessionId;
use support::hydration::empty_hydration_answer;
use support::sync_reconnect::{
    SESSION, TAB, open_link_awaiting_hydration, open_ready_link, session_event,
};

/// The delivery sequence the frames below arrive under. Non-zero because an
/// application frame with no sequence is refused for a different reason, and
/// this file is not about that one.
const SEQ: u64 = 7;

fn the_session() -> SessionId {
    SessionId::try_from(SESSION).expect("a valid session id")
}

/// Answer exactly the hydration calls whose domain is `wanted`, the way a
/// coordinator with no rows answers them.
fn answer_hydration_for(core: &mut ClientCore, effects: &[Effect], wanted: SyncDomain) {
    for effect in effects {
        let Effect::Rpc(call) = effect else {
            continue;
        };
        let belongs = matches!(
            (call, wanted),
            (RpcCall::SessionsList { .. }, SyncDomain::Terminal)
                | (RpcCall::WorkersList { .. }, SyncDomain::Workers)
        );
        if !belongs {
            continue;
        }
        if let Some(answer) = empty_hydration_answer(call) {
            core.handle(ClientEvent::RpcResultReceived(answer));
        }
    }
}

/// Kill the fixture's socket so the next link takes a NEW generation rather
/// than replacing a live one.
fn close_fixture_link(core: &mut ClientCore, generation: u64) {
    core.handle(ClientEvent::SyncLinkClosed {
        generation,
        close_code: Some(1006),
        close_reason: String::new(),
    });
}

#[test]
fn a_snapshot_that_lands_after_a_live_event_does_not_delete_the_live_session() {
    let mut core = ClientCore::in_memory(TAB);
    let first = open_ready_link(&mut core, "sock-one");
    core.handle(ClientEvent::SyncFrameReceived {
        generation: first,
        delivery_seq: SEQ,
        frame: session_event(42),
    });
    assert!(
        core.store().sessions.session(&the_session()).is_some(),
        "the fixture must start with the session the live feed delivered"
    );

    // The socket dies and a new generation opens in its place: fresh domain
    // generations, nothing ready, and a terminal snapshot still to come.
    close_fixture_link(&mut core, first);
    // `hydrated` is the hold. The first socket set it and nothing in this
    // sequence sets it again, so this is the assertion that says a hold is per
    // LINK — checked after the symptom rather than before it, so the failure a
    // regression produces is the lost row and not the bookkeeping.
    let (second, effects) = open_link_awaiting_hydration(&mut core, "sock-two");

    // A session event arrives off the live feed while that snapshot is in
    // flight, and then the snapshot answers WITHOUT the session: a coordinator
    // whose read has not caught up with its own feed says exactly this.
    core.handle(ClientEvent::SyncFrameReceived {
        generation: second,
        delivery_seq: SEQ,
        frame: session_event(43),
    });
    answer_hydration_for(&mut core, &effects, SyncDomain::Terminal);

    assert!(core.store().sync.domain_is_ready(SyncDomain::Terminal));
    assert!(
        core.store().sessions.session(&the_session()).is_some(),
        "a snapshot that predates a folded event must not prune it"
    );
    assert!(
        core.store().hydrated,
        "and the hold it passed is shut again for this socket"
    );
}

#[test]
fn a_session_event_is_not_admissible_on_another_domain_s_readiness() {
    // The session plane is the terminal domain's snapshot and its deltas. While
    // that domain is still waiting for its snapshot, a session event is only
    // admissible because the store's gate reads "some domain is ready" — and
    // the snapshot that seeds the plane then replaces the plane under it.
    let mut core = ClientCore::in_memory(TAB);
    let (_generation, effects) = open_link_awaiting_hydration(&mut core, "sock-one");
    assert_eq!(
        session_event(1).domain(),
        Some(SyncDomain::Terminal),
        "a session event is terminal-domain traffic"
    );
    assert_eq!(
        SyncFrame::SessionsSnapshot {
            sessions: Default::default()
        }
        .domain(),
        Some(SyncDomain::Terminal),
        "the sessions snapshot is the terminal domain's own"
    );

    // The workers domain publishes; the terminal domain has not.
    answer_hydration_for(&mut core, &effects, SyncDomain::Workers);
    assert!(core.store().sync.domain_is_ready(SyncDomain::Workers));
    assert!(!core.store().sync.domain_is_ready(SyncDomain::Terminal));
    assert!(
        !core.store().sync.may_apply(&session_event(1)),
        "a workers domain being ready must not admit a session event"
    );
}
