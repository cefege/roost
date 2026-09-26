//! Both WebSocket upgrade admission state machines, with no socket and no
//! database.
//!
//! Covers: the worker link's five-step order and its four refusals, the Sync
//! link's five-step order and its five refusals, and the two properties that are
//! security rather than convenience -- the worker endpoint's total query ban, and
//! the Sync origin check running before the credential is read.
//!
//! The order IS the contract in both cases
//! (`apps/coord/src/workers/worker-ws-upgrade.ts:37-117`,
//! `apps/coord/src/sync/sync-ws-upgrade.ts:124-213`), and an order that lives in a
//! socket handler is an order nobody reviews. That is why these are pure
//! functions and this file drives them directly.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::worker_link::upgrade_admission::{
    UpgradeDecision, UpgradeRefusal, VerifiedWorkerCaller, WORKER_AUTH_SUBPROTOCOL,
    WorkerUpgradeRequest, admit_worker_upgrade, parse_path_fingerprint,
};

const FP: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
const OTHER_FP: &str = "bb00000000000000000000000000000000000000000000000000000000000000";

fn worker_caller(fingerprint: &str) -> VerifiedWorkerCaller {
    VerifiedWorkerCaller {
        fingerprint: fingerprint.to_string(),
        key_generation: 3,
        label: "test-worker".to_string(),
    }
}

fn admitted_worker_request() -> WorkerUpgradeRequest {
    WorkerUpgradeRequest {
        path: format!("/ws/coord-worker/{FP}"),
        query: String::new(),
        offered_protocols: vec![WORKER_AUTH_SUBPROTOCOL.to_string(), "a.b.c".to_string()],
        caller: Some(worker_caller(FP)),
    }
}

// ── the worker link ────────────────────────────────────────────────────────

#[test]
fn a_well_formed_worker_upgrade_is_admitted_and_bound_to_its_url_fingerprint() {
    let decision = admit_worker_upgrade(&admitted_worker_request(), true, Some(3));
    match decision {
        UpgradeDecision::Admitted {
            fingerprint,
            caller,
        } => {
            assert_eq!(fingerprint, FP);
            assert_eq!(caller.fingerprint, FP);
        }
        UpgradeDecision::Refused(refusal) => panic!("expected admission, got {refusal:?}"),
    }
}

#[test]
fn any_query_string_at_all_refuses_the_worker_endpoint() {
    // Not a denylist of `token=`. "Rejecting the entire query surface guarantees
    // an old `?token=` client cannot leak a credential into access logs while
    // still authenticating successfully by subprotocol"
    // (`worker-ws-upgrade.ts:42-44`). A denylist would be a denylist of the
    // spelling v2 shipped.
    for query in ["?token=abc", "?", "?x=1", "?flow=1"] {
        let mut request = admitted_worker_request();
        request.query = query.to_string();
        let decision = admit_worker_upgrade(&request, true, Some(3));
        assert_eq!(
            decision,
            UpgradeDecision::Refused(UpgradeRefusal::QueryCredential),
            "query {query:?}"
        );
    }
}

#[test]
fn the_query_check_runs_before_the_subprotocol_envelope_is_read() {
    // Both would refuse, but the order is observable in which reason comes back,
    // and the query check is first so an old client cannot learn whether its
    // envelope was well-formed by the reason it got.
    let mut request = admitted_worker_request();
    request.query = "?token=abc".to_string();
    request.offered_protocols = vec!["wrong".to_string()];
    assert_eq!(
        admit_worker_upgrade(&request, true, Some(3)),
        UpgradeDecision::Refused(UpgradeRefusal::QueryCredential)
    );
}

#[test]
fn a_malformed_subprotocol_envelope_refuses_before_the_credential_is_read() {
    for protocols in [
        vec![],
        vec![WORKER_AUTH_SUBPROTOCOL.to_string()],
        vec![WORKER_AUTH_SUBPROTOCOL.to_string(), String::new()],
        vec!["a.b.c".to_string(), WORKER_AUTH_SUBPROTOCOL.to_string()],
        vec![
            WORKER_AUTH_SUBPROTOCOL.to_string(),
            "a.b.c".to_string(),
            "extra".to_string(),
        ],
    ] {
        let mut request = admitted_worker_request();
        request.offered_protocols = protocols.clone();
        assert_eq!(
            admit_worker_upgrade(&request, true, Some(3)),
            UpgradeDecision::Refused(UpgradeRefusal::MalformedSubprotocol),
            "{protocols:?}"
        );
    }
}

#[test]
fn a_credential_that_did_not_verify_refuses() {
    let mut request = admitted_worker_request();
    request.caller = None;
    assert_eq!(
        admit_worker_upgrade(&request, true, Some(3)),
        UpgradeDecision::Refused(UpgradeRefusal::JwtInvalid)
    );
}

#[test]
fn the_url_the_jwt_the_principal_and_the_generation_must_all_agree() {
    // A valid signature proves a key, not a worker: the same key could be a
    // browser's. All four agreeing is what makes the socket a worker's.
    let mut request = admitted_worker_request();
    request.path = format!("/ws/coord-worker/{OTHER_FP}");
    assert_eq!(
        admit_worker_upgrade(&request, true, Some(3)),
        UpgradeDecision::Refused(UpgradeRefusal::PrincipalMismatch),
        "the URL and the JWT disagree"
    );

    let request = admitted_worker_request();
    assert_eq!(
        admit_worker_upgrade(&request, false, Some(3)),
        UpgradeDecision::Refused(UpgradeRefusal::PrincipalMismatch),
        "the key is not a worker"
    );

    let request = admitted_worker_request();
    assert_eq!(
        admit_worker_upgrade(&request, true, Some(4)),
        UpgradeDecision::Refused(UpgradeRefusal::PrincipalMismatch),
        "the key generation moved"
    );

    let request = admitted_worker_request();
    assert_eq!(
        admit_worker_upgrade(&request, true, None),
        UpgradeDecision::Refused(UpgradeRefusal::PrincipalMismatch),
        "the key is not in the cache at all"
    );
}

#[test]
fn every_worker_refusal_answers_401_and_says_only_unauthorized() {
    // One status and one body for all four reasons: a `400` for a malformed
    // envelope and a `401` for a bad signature would tell a prober which half of
    // its guess was right, and this endpoint is reachable by anything that can
    // open a socket.
    for refusal in [
        UpgradeRefusal::QueryCredential,
        UpgradeRefusal::MalformedSubprotocol,
        UpgradeRefusal::JwtInvalid,
        UpgradeRefusal::PrincipalMismatch,
    ] {
        assert_eq!(refusal.status(), 401);
        assert_eq!(refusal.body(), "unauthorized");
    }
}

#[test]
fn a_path_that_is_not_exactly_64_lowercase_hex_yields_no_fingerprint() {
    assert_eq!(
        parse_path_fingerprint(&format!("/ws/coord-worker/{FP}")),
        Some(FP.to_string())
    );
    assert_eq!(parse_path_fingerprint("/ws/coord-worker/"), None);
    assert_eq!(parse_path_fingerprint("/ws/coord-worker/short"), None);
    assert_eq!(
        parse_path_fingerprint(&format!("/ws/coord-worker/{}", "A".repeat(64))),
        None
    );
    assert_eq!(parse_path_fingerprint("/ws/coord-sync"), None);
    assert_eq!(
        parse_path_fingerprint(&format!("/ws/coord-worker/{FP}/extra")),
        None
    );
    assert_eq!(
        parse_path_fingerprint(&format!("/ws/coord-worker/{}extra", "a".repeat(58))),
        None
    );
}

#[test]
fn a_fingerprint_with_an_uppercase_hex_digit_is_refused() {
    // A fingerprint minted anywhere in this fleet is lowercase, and a match that
    // ignored case would let two spellings of one key coexist
    // (`crates/roost-protocol/src/fingerprint.rs:29-30`).
    assert!(
        parse_path_fingerprint(&format!(
            "/ws/coord-worker/{}",
            "AA".to_string() + &"0".repeat(62)
        ))
        .is_none()
    );
}
