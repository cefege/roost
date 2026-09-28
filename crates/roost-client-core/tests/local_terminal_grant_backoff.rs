//! A direct-terminal mint that is refused or rejected: the retry window a
//! refusal arms, and the immediate re-mint a rejection earns.
//! The scope half of the lifecycle is in `local_terminal_grant_scope` and the
//! auth/retirement boundaries in `local_terminal_grant_fences`; the shared
//! session table and the mint-completing harness live in
//! `local_terminal_grants_support`.

mod local_terminal_grants_support;

use roost_client_core::client::local::grants::LOCAL_TERMINAL_GRANT_RETRY_MS;
use roost_client_core::client::local::{GrantRefresh, GrantRefreshReason};

use local_terminal_grants_support::{Harness, open_mint};

#[test]
fn a_refused_mint_is_backed_off_and_cleared_when_sync_returns() {
    let mut harness = Harness::new();
    let request = open_mint(&mut harness);
    let refused = Err("coordinator unavailable".to_string());
    let sessions = &harness.sessions;
    assert_eq!(
        harness.owner.complete_mint(&request, refused, sessions, 0),
        GrantRefresh::Standing(None),
        "a refusal keeps the standing answer at no grant"
    );

    harness.now_ms = LOCAL_TERMINAL_GRANT_RETRY_MS - 1;
    harness.demand("worker-a", "session-a2", true);
    let asked = harness.requests.len();
    assert_eq!(asked, 1, "before the retry deadline nothing mints again");

    // Sync coming back clears the backoff BEFORE the deadline and re-asks. The
    // trigger is the REASON, not new demand: a repeated demand for a session
    // already wanted is a no-op, exactly as it is in v2.
    harness.owner.clear_retry(Some("worker-a"));
    let sessions = &harness.sessions;
    let reason = GrantRefreshReason::SyncConnected;
    let decision = harness
        .owner
        .refresh("worker-a", reason, sessions, harness.now_ms);
    harness.settle(decision);
    assert_eq!(harness.requests.len(), 2, "a cleared window mints at once");
    assert_eq!(
        harness.requests[1].session_ids,
        vec!["session-a", "session-a2"],
        "and the retried request still names the exact sessions"
    );
    // Only a REFUSAL arms a backoff, so that successful mint armed nothing.
    harness.demand("worker-a", "session-b", true);
    assert_eq!(
        harness.requests.len(),
        2,
        "an ungrantable session mints nothing"
    );
}

#[test]
fn a_rejected_credential_clears_the_grant_and_is_re_minted_immediately() {
    let mut harness = Harness::new();
    harness.demand("worker-a", "session-a", true);
    harness.owner.take_publications();
    harness.owner.drop("worker-a");
    assert_eq!(
        harness.owner.current("worker-a"),
        None,
        "a credential the worker rejected is not kept for a retry"
    );
    let published = harness.owner.take_publications();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].grant, None, "the grant is announced as gone");

    // A REJECTION is not a refusal, so nothing here is inside a backoff.
    harness.demand("worker-a", "session-a2", true);
    assert_eq!(harness.requests.len(), 2);
    assert!(
        harness.owner.current("worker-a").is_some(),
        "a grant is back"
    );
}
