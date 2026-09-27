//! The local terminal loopback end to end: the handshake a worker's `Ready` must
//! pass, the credential that opens the door, the grant's scope and lifetime, and
//! the route a session's input is elected onto.
//!
//! Ported from `apps/web/tests/localTerminal.test.ts`, which drives a fake socket
//! because the v2 class owns one. This port drives the same rules as values, so
//! the two cases the v2 test exists for are the two that still matter: a rolling
//! worker gets a fresh per-connection namespace, and a HALF-present rolling tuple
//! is refused rather than guessed at. v2's mock of the input router is replaced
//! by the real `InputRouter`, so "closing retires the token" is proved by the
//! lane's outcome. Where a door is dialled AT is discovery's question.

use std::collections::{BTreeMap, BTreeSet};

use roost_client_core::client::local::door::{
    LoopbackReady, ReadyRefusal, SecretUseLedger, admit_ready,
};
use roost_client_core::client::local::grants::GrantOwner;
use roost_client_core::client::local::outbound::{
    SyncTerminalState, destination_for_session, loopback_ready_effects,
};
use roost_client_core::client::local::{
    GrantMintAnswer, GrantRefresh, GrantRefusal, GrantSessionFact, LocalTerminalGrant,
};
use roost_client_core::{
    DirectCarrier, DirectCommand, Effect, InputRouter, PromotionCandidate, RouteRegistry,
    TerminalSession, TerminalToken, TerminalTransport,
};

/// The connection id a host would mint for one socket.
const CONNECTION: &str = "connection-1";

/// v2's fixture answer: an epoch and both capabilities advertised.
fn answer() -> GrantMintAnswer {
    GrantMintAnswer {
        grant_id: "grant-a".to_string(),
        secret: "secret-a".to_string(),
        ttl_ms: 43_200_000,
        worker_epoch: "epoch-a".to_string(),
        peer_supported: true,
        stun_urls: Vec::new(),
        input_route_supported: true,
    }
}

fn grant() -> LocalTerminalGrant {
    grant_for(BTreeSet::from(["session-a".to_string()]), "grant-a")
}

fn grant_for(session_ids: BTreeSet<String>, grant_id: &str) -> LocalTerminalGrant {
    let answer = GrantMintAnswer {
        grant_id: grant_id.to_string(),
        ..answer()
    };
    LocalTerminalGrant::from_answer(answer, "worker-a", session_ids, "tab-a", "device-a", 0)
        .expect("the fixture answer carries both a grant id and a secret")
}

/// v2's `readyFrame` defaults: a worker reporting NEITHER an epoch nor a socket
/// id, which is what makes it rolling.
fn ready() -> LoopbackReady {
    LoopbackReady {
        worker_fingerprint: "worker-a".to_string(),
        session_ids: BTreeSet::from(["session-a".to_string()]),
        socket_generation: 7,
        worker_epoch: String::new(),
        socket_id: String::new(),
        peer_id: String::new(),
    }
}

#[test]
fn uses_a_fresh_browser_namespace_only_for_a_complete_old_ready_tuple() {
    let admission = admit_ready(&grant(), "worker-a", CONNECTION, &ready())
        .expect("a complete old Ready tuple is admitted");

    assert_eq!(admission.token.transport, TerminalTransport::Loopback);
    // v2 asserts `processEpoch === socketId`; v3's token has no socket id at all,
    // so the same property reads as "the namespace is this connection's own id".
    assert_eq!(admission.token.process_epoch, CONNECTION);
    assert_eq!(admission.token.socket_generation, 7);
    assert!(admission.allows_session("session-a"), "the Ready named it");
    // A rolling worker reports no epoch, so there is nothing to claim a route
    // against and the grant's advertised capability must NOT be believed here.
    assert!(!admission.input_route_supported, "a rolling worker names no epoch");

    // And the namespace is what a close retires: a batch started on this token
    // settles `ambiguous`, because the carrier may have handed the bytes over.
    let mut router = InputRouter::new();
    let pending = router
        .admit("session-a", Some("view-1".into()), b"ls\n".to_vec(), 0)
        .expect("an open lane admits one batch");
    assert!(router.mark_started(pending.input_seq, &admission.token));
    let settled = router.retire_token(&admission.token, "local terminal closed");
    assert_eq!(settled.len(), 1, "the batch on this token settles exactly once");
    assert!(
        settled[0].is_ambiguous(),
        "input handed to a closing carrier is ambiguous, never retried"
    );
}

#[test]
fn rejects_peer_ready_fields_or_a_half_present_rolling_tuple() {
    let half = LoopbackReady {
        worker_epoch: "epoch-a".to_string(),
        ..ready()
    };
    assert_eq!(
        admit_ready(&grant(), "worker-a", CONNECTION, &half),
        Err(ReadyRefusal::HalfPresentTuple),
        "an epoch with no socket id is a half-present tuple, not a rolling worker"
    );

    let peer = LoopbackReady {
        peer_id: "11111111-1111-4111-8111-111111111111".to_string(),
        ..ready()
    };
    assert_eq!(
        admit_ready(&grant(), "worker-a", CONNECTION, &peer),
        Err(ReadyRefusal::PeerIdPresent),
        "a peer handshake arriving on the loopback path is refused"
    );

    let other_worker = LoopbackReady {
        worker_fingerprint: "worker-b".to_string(),
        ..ready()
    };
    assert_eq!(
        admit_ready(&grant(), "worker-a", CONNECTION, &other_worker),
        Err(ReadyRefusal::WorkerMismatch),
        "a worker that is not this door's may not answer for it"
    );

    let ungranted = LoopbackReady {
        session_ids: BTreeSet::from(["session-z".to_string()]),
        ..ready()
    };
    assert_eq!(
        admit_ready(&grant(), "worker-a", CONNECTION, &ungranted),
        Err(ReadyRefusal::SessionNotGranted),
        "a session the grant does not name is not the worker's to admit"
    );

    assert_eq!(
        admit_ready(&grant(), "worker-b", CONNECTION, &ready()),
        Err(ReadyRefusal::GrantNamesAnotherWorker),
        "a grant minted for another worker opens no door here"
    );
}

#[test]
fn keeps_actual_worker_epoch_and_socket_id_on_a_current_ready_tuple() {
    let current = LoopbackReady {
        worker_epoch: "epoch-a".to_string(),
        socket_id: "socket-a".to_string(),
        ..ready()
    };
    let admission = admit_ready(&grant(), "worker-a", CONNECTION, &current)
        .expect("a current Ready tuple is admitted");

    assert_eq!(admission.token.process_epoch, "epoch-a");
    assert_eq!(admission.token.worker_fp.as_deref(), Some("worker-a"));
    assert_eq!(admission.token.domain_generation, 7);
    assert!(admission.input_route_supported, "a named epoch can be claimed against");
}

#[test]
fn extends_a_current_grant_in_place_but_refuses_a_narrowed_scope() {
    let current = LoopbackReady {
        worker_epoch: "epoch-a".to_string(),
        socket_id: "socket-a".to_string(),
        ..ready()
    };
    let mut admission = admit_ready(&grant(), "worker-a", CONNECTION, &current)
        .expect("a current Ready tuple is admitted");
    let token = admission.token.clone();
    let widened = grant_for(
        BTreeSet::from(["session-a".to_string(), "session-b".to_string()]),
        "grant-a2",
    );

    assert!(admission.extend_grant(&widened));
    assert_eq!(
        admission.token, token,
        "widening a grant never changes the generation the carrier presents"
    );
    assert!(admission.allows_session("session-b"));

    assert!(
        !admission.extend_grant(&grant()),
        "narrowing is refused: the worker is already serving session-b"
    );
}

#[test]
fn an_admitted_carrier_asks_for_one_baseline_per_ready_session_in_order() {
    let current = LoopbackReady {
        worker_epoch: "epoch-a".to_string(),
        socket_id: "socket-a".to_string(),
        session_ids: BTreeSet::from(["session-a".to_string(), "session-b".to_string()]),
        ..ready()
    };
    let both = BTreeSet::from(["session-a".to_string(), "session-b".to_string()]);
    let two_sessions = grant_for(both, "grant-a");
    let admission = admit_ready(&two_sessions, "worker-a", CONNECTION, &current)
        .expect("a Ready may only name sessions the grant names");

    let baseline = |session_id: &str| Effect::SendDirect {
        token: admission.token.clone(),
        command: DirectCommand::Resync {
            session_id: session_id.to_string(),
            view_id: String::new(),
        },
    };
    assert_eq!(
        loopback_ready_effects(&admission),
        vec![baseline("session-a"), baseline("session-b")],
        "one baseline per admitted session, in session-id order, every run"
    );
}

/// Both halves of the scope rule, on ONE grant, so neither can satisfy the other.
#[test]
fn an_expired_grant_is_refused_and_an_out_of_scope_session_is_refused_on_the_same_grant() {
    let grant = grant();
    let (live, expired) = (0, grant.expires_at_ms);
    assert_eq!(
        grant.admits("session-a", live),
        Ok(()),
        "the named session is admitted inside the grant's lifetime"
    );
    assert_eq!(
        grant.admits("session-b", live),
        Err(GrantRefusal::OutOfScope),
        "an unexpired grant naming no such session is refused for SCOPE"
    );
    assert_eq!(
        grant.admits("session-a", expired),
        Err(GrantRefusal::Expired),
        "the same session on the same grant is refused once its TTL runs out"
    );
    assert_eq!(
        grant.admits("session-a", expired - 1),
        Ok(()),
        "one millisecond before the deadline the grant is still good"
    );
}

/// The single-use rule: the worker's door answers a replay by REPLACING the
/// socket it already handed out, so the ledger refuses the second dial instead.
#[test]
fn a_secret_cannot_be_replayed() {
    let grant = grant();
    let mut ledger = SecretUseLedger::new();

    assert_eq!(ledger.claim(&grant), Ok(()));
    assert!(ledger.holds(&grant.secret));
    assert_eq!(
        ledger.claim(&grant),
        Err(GrantRefusal::SecretInUse),
        "a second socket on one secret is the replay the door replaces"
    );
    ledger.release(&grant.secret);
    assert_eq!(
        ledger.claim(&grant),
        Ok(()),
        "closing the socket returns the secret, so a redial is not a replay"
    );
    ledger.release_worker("worker-a");
    assert_eq!(ledger.claim(&grant), Ok(()), "retiring a worker releases them");
}

#[test]
fn a_renewed_grant_carries_a_new_secret_and_never_prints_either() {
    let first = grant();
    let renewed = LocalTerminalGrant::from_answer(
        GrantMintAnswer {
            grant_id: "grant-a2".to_string(),
            secret: "secret-a-2".to_string(),
            worker_epoch: "epoch-worker-a".to_string(),
            peer_supported: false,
            stun_urls: vec!["stun:stun.example:3478".to_string()],
            ..answer()
        },
        "worker-a",
        first.session_ids.clone(),
        "tab-a",
        "device-a",
        0,
    )
    .expect("a fresh answer carries a fresh secret");

    let mut ledger = SecretUseLedger::new();
    assert_eq!(ledger.claim(&first), Ok(()));
    assert_eq!(
        ledger.claim(&renewed),
        Ok(()),
        "a renewal is a new coordinator mint, not a replay of the old one"
    );
    assert!(
        renewed.stun_urls.is_empty(),
        "an answer denying peer support must not hand out STUN servers"
    );

    let rendered = format!("{first:?} {renewed:?}");
    assert!(
        !rendered.contains("secret-a"),
        "a grant is logged freely; a secret must never reach it: {rendered}"
    );
}

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
    assert!(chosen.input_route_supported, "a live grant carries an epoch");
    assert_eq!(
        elect(expired),
        None,
        "a carrier whose grant has run out is gone: input is refused rather \
         than moved to Sync, which would hide the route loss from the client"
    );
}
