//! The SYNC WebSocket's upgrade admission, with no socket and no database.
//!
//! Covers: the five-step order in `apps/coord/src/sync/sync-ws-upgrade.ts:124-213`,
//! its five refusals with their exact statuses, the origin policy's exact-match
//! rule, and the scope a browser and a worker socket each get. The worker
//! endpoint's order is in `upgrade_admission.rs`.
//!
//! The origin-before-credential order is the security property here: a browser can
//! be made to open a WebSocket to a loopback address from any page it visits, and
//! after the credential a rejected origin and a rejected credential look identical
//! to the page.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::sync_ws::upgrade_admission::{
    CONNECTION_REJECTION_CLOSE_CODE, CONNECTION_REJECTION_REASON, OriginPolicy, PrincipalKind,
    SYNC_AUTH_SUBPROTOCOL, SYNC_QUERY_FLOW_V1, SYNC_QUERY_V2, SYNC_WS_PATH, SyncUpgradeDecision,
    SyncUpgradeRefusal, SyncUpgradeRequest, VerifiedSyncCaller, admit_sync_upgrade,
};
use roost_coord::worker_link::upgrade_admission::WORKER_AUTH_SUBPROTOCOL;

const FP: &str = "aa00000000000000000000000000000000000000000000000000000000000000";

fn policy() -> OriginPolicy {
    OriginPolicy {
        public_url: Some("https://roost.example".to_string()),
        web_public_url: None,
        cors_allowed_origins: vec!["https://app.example".to_string()],
        worker_local_ui_origin: "http://127.0.0.1:4104".to_string(),
        loopback_bind: None,
        relaxed_csp: false,
    }
}

fn sync_request() -> SyncUpgradeRequest {
    SyncUpgradeRequest {
        path: SYNC_WS_PATH.to_string(),
        origin: Some("https://roost.example".to_string()),
        host: "roost.example".to_string(),
        offered_protocols: vec![SYNC_AUTH_SUBPROTOCOL.to_string(), "a.b.c".to_string()],
        caller: Some(VerifiedSyncCaller {
            fingerprint: FP.to_string(),
            label: "browser".to_string(),
        }),
        tab: Some("tab-1".to_string()),
        since: Some("42".to_string()),
        flow: Some(SYNC_QUERY_FLOW_V1.to_string()),
        sync_v: Some(SYNC_QUERY_V2.to_string()),
    }
}

#[test]
fn a_well_formed_sync_upgrade_is_admitted_with_its_scope() {
    let decision = admit_sync_upgrade(&sync_request(), &policy(), PrincipalKind::AccountDevice);
    let scope = match decision {
        SyncUpgradeDecision::Admitted { scope, .. } => scope,
        SyncUpgradeDecision::Refused(refusal) => panic!("expected admission, got {refusal:?}"),
    };
    assert!(!scope.read_only, "a browser socket is not read-only");
    assert_eq!(scope.tab_id.as_deref(), Some("tab-1"));
    assert_eq!(
        scope.viewer_key.as_deref(),
        Some(&format!("{FP}:tab-1")[..])
    );
    assert!(scope.flow_control);
    assert!(scope.domain_generations);
    assert_eq!(scope.since_event_id, 42);
}

#[test]
fn a_worker_principal_gets_a_read_only_socket_over_its_own_scope() {
    // "Worker Sync is a firehose consumer only; it may ACK/subscription-control
    // delivery but cannot issue terminal view or input commands"
    // (`sync-ws-upgrade.ts:78-83`).
    let decision = admit_sync_upgrade(
        &sync_request(),
        &policy(),
        PrincipalKind::Worker(FP.to_string()),
    );
    let scope = match decision {
        SyncUpgradeDecision::Admitted { scope, .. } => scope,
        SyncUpgradeDecision::Refused(refusal) => panic!("expected admission, got {refusal:?}"),
    };
    assert!(scope.read_only);
    assert_eq!(scope.owner_worker_fp.as_deref(), Some(FP));
    assert_eq!(
        scope.tab_id, None,
        "a read-only socket owns no view handles"
    );
    assert_eq!(scope.viewer_key, None);
}

#[test]
fn the_origin_check_runs_before_the_credential_is_read() {
    // A browser can be made to open a WebSocket to a loopback address from any
    // page it visits, and the credential rides in a subprotocol the page cannot
    // set. After the credential, a rejected origin and a rejected credential look
    // identical to the page and it cannot tell which to fix.
    let mut request = sync_request();
    request.origin = Some("https://evil.example".to_string());
    request.caller = None;
    assert_eq!(
        admit_sync_upgrade(&request, &policy(), PrincipalKind::AccountDevice),
        SyncUpgradeDecision::Refused(SyncUpgradeRefusal::OriginRejected)
    );
}

#[test]
fn an_absent_origin_is_allowed_because_it_is_not_the_thing_this_check_is_against() {
    let mut request = sync_request();
    request.origin = None;
    assert!(matches!(
        admit_sync_upgrade(&request, &policy(), PrincipalKind::AccountDevice),
        SyncUpgradeDecision::Admitted { .. }
    ));
}

#[test]
fn a_declared_origin_and_the_https_twin_of_the_host_are_both_allowed() {
    for origin in [
        "https://roost.example",
        "https://app.example",
        "http://127.0.0.1:4104",
    ] {
        let mut request = sync_request();
        request.origin = Some(origin.to_string());
        assert!(
            matches!(
                admit_sync_upgrade(&request, &policy(), PrincipalKind::AccountDevice),
                SyncUpgradeDecision::Admitted { .. }
            ),
            "{origin}"
        );
    }
}

#[test]
fn the_loopback_http_twin_needs_a_loopback_bind_and_a_loopback_host() {
    // "A local-first coordinator serves its canonical browser origin over HTTP"
    // (`sync-ws-upgrade.ts:54-60`). Admitting it on a routable bind would let
    // any page on the network open the socket.
    let mut request = sync_request();
    request.origin = Some("http://roost.example".to_string());
    request.host = "roost.example".to_string();

    let mut no_bind = policy();
    assert_eq!(
        admit_sync_upgrade(&request, &no_bind, PrincipalKind::AccountDevice),
        SyncUpgradeDecision::Refused(SyncUpgradeRefusal::OriginRejected),
        "no bind known"
    );

    no_bind.loopback_bind = Some("0.0.0.0:4113".to_string());
    assert_eq!(
        admit_sync_upgrade(&request, &no_bind, PrincipalKind::AccountDevice),
        SyncUpgradeDecision::Refused(SyncUpgradeRefusal::OriginRejected),
        "a routable bind must not admit the plain-HTTP twin"
    );
}

#[test]
fn a_legacy_self_hosted_key_is_404_and_not_401() {
    // It authenticates. What it lacks is a SCOPE, and the two answers say
    // different things: 404 says there is nothing here, which is true; 401 would
    // say the key is not valid, which is false and would send an operator to
    // re-pair a key that works everywhere else
    // (`sync-ws-upgrade.ts:160-170`).
    let decision = admit_sync_upgrade(&sync_request(), &policy(), PrincipalKind::LegacySelfHosted);
    assert_eq!(
        decision,
        SyncUpgradeDecision::Refused(SyncUpgradeRefusal::LegacySelfHosted)
    );
    assert_eq!(SyncUpgradeRefusal::LegacySelfHosted.status(), 404);
    assert_eq!(SyncUpgradeRefusal::LegacySelfHosted.body(), "not found");
}

#[test]
fn an_oversized_tab_refuses_with_the_connection_rejection_reason() {
    let mut request = sync_request();
    request.tab = Some("t".repeat(257));
    let decision = admit_sync_upgrade(&request, &policy(), PrincipalKind::AccountDevice);
    assert_eq!(
        decision,
        SyncUpgradeDecision::Refused(SyncUpgradeRefusal::TabTooLong)
    );
    assert_eq!(SyncUpgradeRefusal::TabTooLong.status(), 400);
    assert_eq!(
        SyncUpgradeRefusal::TabTooLong.body(),
        CONNECTION_REJECTION_REASON
    );
    assert_eq!(CONNECTION_REJECTION_CLOSE_CODE, 1013);

    // Exactly at the bound is inside it.
    request.tab = Some("t".repeat(256));
    assert!(matches!(
        admit_sync_upgrade(&request, &policy(), PrincipalKind::AccountDevice),
        SyncUpgradeDecision::Admitted { .. }
    ));
}

#[test]
fn a_blank_tab_is_treated_as_absent_so_the_socket_is_read_only_by_construction() {
    let mut request = sync_request();
    request.tab = Some("   ".to_string());
    let scope = match admit_sync_upgrade(&request, &policy(), PrincipalKind::AccountDevice) {
        SyncUpgradeDecision::Admitted { scope, .. } => scope,
        SyncUpgradeDecision::Refused(refusal) => panic!("expected admission, got {refusal:?}"),
    };
    assert_eq!(scope.tab_id, None);
    assert_eq!(
        scope.viewer_key, None,
        "no tab means no socket-bound view owner"
    );
}

#[test]
fn domain_generations_need_both_query_values_and_not_either_alone() {
    let mut request = sync_request();
    request.flow = None;
    request.sync_v = Some(SYNC_QUERY_V2.to_string());
    let scope = match admit_sync_upgrade(&request, &policy(), PrincipalKind::AccountDevice) {
        SyncUpgradeDecision::Admitted { scope, .. } => scope,
        SyncUpgradeDecision::Refused(refusal) => panic!("expected admission, got {refusal:?}"),
    };
    assert!(!scope.flow_control);
    assert!(
        !scope.domain_generations,
        "sync_v=2 alone must not negotiate generations"
    );

    let mut request = sync_request();
    request.sync_v = None;
    let scope = match admit_sync_upgrade(&request, &policy(), PrincipalKind::AccountDevice) {
        SyncUpgradeDecision::Admitted { scope, .. } => scope,
        SyncUpgradeDecision::Refused(refusal) => panic!("expected admission, got {refusal:?}"),
    };
    assert!(
        scope.flow_control,
        "flow=1 alone is the ACK window without generations"
    );
    assert!(!scope.domain_generations);
}

#[test]
fn an_unparseable_since_reads_as_zero_rather_than_failing_the_upgrade() {
    // v2's `Number(...) || 0` (`sync-ws-upgrade.ts:179`) treats garbage and
    // absence the same, and a client that sends garbage gets a full seed rather
    // than a refusal it cannot act on.
    let mut request = sync_request();
    request.since = Some("not-a-number".to_string());
    let scope = match admit_sync_upgrade(&request, &policy(), PrincipalKind::AccountDevice) {
        SyncUpgradeDecision::Admitted { scope, .. } => scope,
        SyncUpgradeDecision::Refused(refusal) => panic!("expected admission, got {refusal:?}"),
    };
    assert_eq!(scope.since_event_id, 0);
}

#[test]
fn the_negotiation_literals_are_the_ones_a_client_sends() {
    // A client that connects to the wrong path gets an HTTP error that looks
    // exactly like a down coordinator, and one that offers the wrong subprotocol
    // is refused before authentication is attempted
    // (`crates/roost-protocol/src/wire/sync_ws.rs:35-41`).
    assert_eq!(SYNC_WS_PATH, "/ws/coord-sync");
    assert_eq!(SYNC_AUTH_SUBPROTOCOL, "roost-auth");
    assert_eq!(WORKER_AUTH_SUBPROTOCOL, "roost-worker-auth");
    assert_eq!(SYNC_QUERY_FLOW_V1, "1");
    assert_eq!(SYNC_QUERY_V2, "2");
}
