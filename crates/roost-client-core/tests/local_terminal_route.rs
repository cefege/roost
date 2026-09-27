//! The route a session's input is elected onto: an elected direct carrier wins,
//! and one whose grant has run out is refused rather than quietly moved.
//!
//! Ported from `apps/web/tests/localTerminal.test.ts`, which drives a fake socket
//! because the v2 class owns one. The handshake that admits a carrier is
//! `local_terminal_ready.rs` and the credential's own rules are
//! `local_terminal_credential.rs`; this file starts once a route is registered
//! and a grant is installed, and asks only where the session's input goes.
//! v2's mock of the input router is replaced by the real `InputRouter`, so
//! "closing retires the token" is proved by the lane's outcome. Where a door is
//! dialled AT is discovery's question.
//!
//! Depends on `local_terminal_support` for the door's connection id and mint
//! answer; the three fixtures below are used by no sibling binary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod local_terminal_support;

use local_terminal_support::{CONNECTION, answer};
use roost_client_core::client::local::grants::GrantOwner;
use roost_client_core::client::local::outbound::{SyncTerminalState, destination_for_session};
use roost_client_core::client::local::{GrantRefresh, GrantSessionFact};
use roost_client_core::{
    DirectCarrier, PromotionCandidate, RouteRegistry, TerminalSession, TerminalToken,
    TerminalTransport,
};
use std::collections::{BTreeMap, BTreeSet};

/// The Sync socket as the outbound path reads it, on generation 7.
fn sync(ready: bool) -> SyncTerminalState {
    SyncTerminalState {
        socket_generation: 7,
        socket_id: "socket-a".to_string(),
        process_epoch: "epoch-a".to_string(),
        domain_generation: 3,
        ready,
    }
}

/// One elected loopback route, promoted the way the registry admits one.
fn registry_with_a_loopback_route() -> (RouteRegistry, TerminalToken) {
    let token = TerminalToken::direct(7, TerminalTransport::Loopback, "worker-a", "epoch-a", 7);
    let mut registry = RouteRegistry::new();
    registry.set_view_demand("worker-a", "session-a", "view-1", true);
    let carrier = DirectCarrier {
        connection_id: CONNECTION.into(),
        worker_fp: "worker-a".into(),
        transport: TerminalTransport::Loopback,
        token: token.clone(),
        granted_sessions: BTreeSet::from(["session-a".to_string()]),
    };
    assert!(registry.register(carrier));
    let mut replica = TerminalSession::new("session-a", "worker-a");
    replica.bind_generation(&token);
    let candidate = PromotionCandidate {
        session_id: "session-a".into(),
        connection_id: CONNECTION.into(),
        token: token.clone(),
        attempt_id: 1,
        baseline_ready: true,
    };
    assert!(registry.stage(candidate, replica));
    assert!(registry.promote("session-a", 1, &token).is_ok());
    (registry, token)
}

/// A `GrantOwner` holding one live grant for `session-a` on `worker-a`.
fn owner_with_a_grant() -> GrantOwner {
    let fact = GrantSessionFact {
        worker_fp: "worker-a".into(),
        open: true,
    };
    let tables = BTreeMap::from([("session-a".to_string(), fact)]);
    let mut owner = GrantOwner::new("tab-a", "device-a");
    let decision = owner.set_demand("worker-a", "session-a", true, &tables, 0);
    let GrantRefresh::Mint(request) = decision else {
        panic!("a wanted open session mints");
    };
    let installed = owner.complete_mint(&request, Ok(answer()), &tables, 0);
    assert!(matches!(installed, GrantRefresh::Installed(_)));
    owner
}

/// An elected route wins; one whose grant has run out REFUSES rather than quietly
/// handing the session's input somewhere else.
#[test]
fn an_elected_direct_route_wins_and_a_dead_grant_refuses_rather_than_falling_back() {
    let owner = owner_with_a_grant();
    let (registry, token) = registry_with_a_loopback_route();
    let expired = owner.current("worker-a").expect("a grant").expires_at_ms;
    let elect = |now_ms| {
        destination_for_session(
            &registry,
            Some(&sync(true)),
            &owner,
            "session-a",
            Some("worker-a"),
            true,
            now_ms,
        )
    };

    let chosen = elect(0).expect("an elected route on a live grant wins");
    assert_eq!(chosen.token, token, "the elected route's own generation");
    assert!(chosen.closeable, "a direct carrier is closable here");
    assert!(
        chosen.input_route_supported,
        "a live grant carries an epoch"
    );
    assert_eq!(
        elect(expired),
        None,
        "a carrier whose grant has run out is gone: input is refused rather \
         than moved to Sync, which would hide the route loss from the client"
    );
}
