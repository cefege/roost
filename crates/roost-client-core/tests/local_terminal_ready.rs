//! The handshake a worker's `Ready` must pass, and what an admitted carrier
//! must then ask for.
//!
//! Ported from `apps/web/tests/localTerminal.test.ts`, which drives a fake socket
//! because the v2 class owns one. This port drives the same rules as values, so
//! the two cases the v2 test exists for are the two that still matter: a rolling
//! worker gets a fresh per-connection namespace, and a HALF-present rolling tuple
//! is refused rather than guessed at. v2's mock of the input router is replaced
//! by the real `InputRouter`, so "closing retires the token" is proved by the
//! lane's outcome. Where a door is dialled AT is discovery's question.
//!
//! Depends on `local_terminal_support` for the door's grant and Ready fixtures.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod local_terminal_support;

use local_terminal_support::{CONNECTION, grant, grant_for, ready};
use roost_client_core::client::local::door::{LoopbackReady, ReadyRefusal, admit_ready};
use roost_client_core::client::local::outbound::loopback_ready_effects;
use roost_client_core::{DirectCommand, Effect, InputRouter, TerminalTransport};
use std::collections::BTreeSet;

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
    assert!(
        !admission.input_route_supported,
        "a rolling worker names no epoch"
    );

    // And the namespace is what a close retires: a batch started on this token
    // settles `ambiguous`, because the carrier may have handed the bytes over.
    let mut router = InputRouter::new();
    let pending = router
        .admit("session-a", Some("view-1".into()), b"ls\n".to_vec(), 0)
        .expect("an open lane admits one batch");
    assert!(router.mark_started(pending.input_seq, &admission.token));
    let settled = router.retire_token(&admission.token, "local terminal closed");
    assert_eq!(
        settled.len(),
        1,
        "the batch on this token settles exactly once"
    );
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
    assert!(
        admission.input_route_supported,
        "a named epoch can be claimed against"
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
        // EMPTY stream and epoch, and that is the request, not a stub: a first
        // baseline for a session this tab has no replica of is "I hold nothing",
        // and an empty position is how a resync says so. A session that HAS a
        // stream names it, which is the case the four fields exist for.
        command: DirectCommand::Resync {
            session_id: session_id.to_string(),
            view_id: String::new(),
            stream_id: String::new(),
            grid_epoch: String::new(),
            seq: 0,
        },
    };
    assert_eq!(
        loopback_ready_effects(&admission),
        vec![baseline("session-a"), baseline("session-b")],
        "one baseline per admitted session, in session-id order, every run"
    );
}
