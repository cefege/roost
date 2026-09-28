//! The credential that opens the loopback door: its scope, its lifetime, its
//! single use, and what a renewal hands over in its place.
//!
//! Ported from `apps/web/tests/localTerminal.test.ts`, which drives a fake socket
//! because the v2 class owns one. The mint and fence lifecycle behind these
//! answers is `local_terminal_grants.rs`; this file asks only what one answer is
//! allowed to do once the browser holds it. The single-use rule is proved by the
//! worker's own behaviour: a replay is answered by REPLACING the socket already
//! handed out, so the ledger refuses the second dial.
//!
//! Depends on `local_terminal_support` for the mint answer and the grants it
//! turns into.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod local_terminal_support;

use local_terminal_support::{CONNECTION, answer, grant, grant_for, ready};
use roost_client_core::client::local::door::{LoopbackReady, SecretUseLedger, admit_ready};
use roost_client_core::client::local::{GrantMintAnswer, GrantRefusal, LocalTerminalGrant};
use std::collections::BTreeSet;

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
    assert_eq!(
        ledger.claim(&grant),
        Ok(()),
        "retiring a worker releases them"
    );
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
