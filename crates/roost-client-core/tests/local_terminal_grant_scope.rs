//! What a direct-terminal grant names, for which worker, and when it is
//! minted: the scope half of the lifecycle.
//! The fences are in `local_terminal_grant_fences` and the backoff in
//! `local_terminal_grant_backoff`; the shared session table and the
//! mint-completing harness live in `local_terminal_grants_support`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod local_terminal_grants_support;

use std::collections::BTreeSet;

use roost_client_core::client::local::{GrantMintRequest, GrantRefresh, GrantSessionFact};

use local_terminal_grants_support::{Harness, open_mint};

#[test]
fn keeps_grants_and_renewals_scoped_to_their_worker() {
    let mut harness = Harness::new();
    harness.demand("worker-a", "session-a", true);
    harness.demand("worker-b", "session-b", true);

    assert_eq!(
        harness.requested("worker-a"),
        vec![vec!["session-a".to_string()]],
        "one mint per worker, naming only that worker's own sessions"
    );
    assert_eq!(
        harness.requested("worker-b"),
        vec![vec!["session-b".to_string()]]
    );
    assert!(
        harness.requests.iter().all(|r| r.tab_id == "tab-test"),
        "bound to this tab"
    );

    harness.renew("worker-a");
    assert_eq!(harness.requests.len(), 3, "a renewal mints once more");
    assert_eq!(
        harness
            .owner
            .current("worker-b")
            .map(|g| g.grant_id.as_str()),
        Some("grant-worker-b-session-b"),
        "a renewal of one worker must not touch another's grant"
    );
}

#[test]
fn coalesces_added_demand_into_a_follow_up_mint() {
    let mut harness = Harness::new();
    let first = open_mint(&mut harness);
    assert_eq!(
        harness.owner.set_demand(
            "worker-a",
            "session-a2",
            true,
            &harness.sessions,
            harness.now_ms,
        ),
        GrantRefresh::Standing(None),
        "a second demand during a mint is a note, not a second concurrent mint"
    );

    let answer = harness.answer_for(&first);
    let sessions = &harness.sessions;
    let now_ms = harness.now_ms;
    let completed = harness
        .owner
        .complete_mint(&first, Ok(answer), sessions, now_ms);
    let GrantRefresh::Mint(follow_up) = completed else {
        panic!("the expanded demand is owed its own mint before it can be used");
    };
    assert_eq!(follow_up.session_ids, vec!["session-a", "session-a2"]);
    harness.requests.push(follow_up.clone());

    let answer = harness.answer_for(&follow_up);
    let sessions = &harness.sessions;
    let installed = harness
        .owner
        .complete_mint(&follow_up, Ok(answer), sessions, now_ms);
    let GrantRefresh::Installed(grant) = installed else {
        panic!("the follow-up mint installs");
    };
    assert_eq!(
        grant.session_ids,
        BTreeSet::from(["session-a".to_string(), "session-a2".to_string()]),
        "the installed grant names the expanded set, not the original one"
    );
    let two = vec!["session-a".to_string(), "session-a2".to_string()];
    assert_eq!(
        harness.requested("worker-a"),
        vec![vec!["session-a".to_string()], two]
    );
}

#[test]
fn retains_an_open_authorized_session_when_demand_moves_within_one_worker() {
    let mut harness = Harness::new();
    harness.demand("worker-a", "session-a", true);
    harness.demand("worker-a", "session-a", false);
    harness.demand("worker-a", "session-a2", true);

    let two = vec!["session-a".to_string(), "session-a2".to_string()];
    assert_eq!(
        harness.requested("worker-a"),
        vec![vec!["session-a".to_string()], two.clone()],
        "a granted session stays in scope when demand moves inside one worker"
    );
    let standing = harness
        .owner
        .current("worker-a")
        .map(|g| g.session_ids.clone());
    let both = BTreeSet::from(["session-a".to_string(), "session-a2".to_string()]);
    assert_eq!(standing, Some(both));
}

#[test]
fn mints_an_initial_grant_without_a_workers_projection() {
    let mut harness = Harness::new();
    // Nothing about `worker-a` is known anywhere: no workers row, no grant, no
    // prior demand. The session table is the only input the mint reads.
    harness.demand("worker-a", "session-a", true);
    assert_eq!(
        harness.requested("worker-a"),
        vec![vec!["session-a".to_string()]]
    );
    let grant = harness
        .owner
        .current("worker-a")
        .expect("the mint installed");
    assert_eq!(grant.tab_id, "tab-test");
    assert_eq!(grant.device_fingerprint, "device-test");
    assert!(harness.requests.iter().all(|r| r.tab_id == "tab-test"));
    assert_eq!(
        grant.expires_at_ms, 43_200_000,
        "the deadline is the coordinator's TTL, counted from arrival"
    );
}

#[test]
fn a_closed_session_or_another_workers_session_is_never_named() {
    let mut harness = Harness::new();
    let shut = GrantSessionFact {
        worker_fp: "worker-a".into(),
        open: false,
    };
    harness.sessions.insert("session-shut".into(), shut);

    harness.demand("worker-a", "session-shut", true);
    harness.demand("worker-a", "session-b", true);

    assert_eq!(harness.owner.current("worker-a"), None);
    assert!(
        harness.requests.is_empty(),
        "a closed session and another worker's session are never named"
    );
}

#[test]
fn a_mint_is_capped_at_the_protocol_maximum_sessions() {
    let fact = GrantSessionFact {
        worker_fp: "worker-a".into(),
        open: true,
    };
    let mut harness = Harness::new();
    let mut first: Option<GrantMintRequest> = None;
    for index in 0..300 {
        let id = format!("session-{index:04}");
        harness.sessions.insert(id.clone(), fact.clone());
        let sessions = &harness.sessions;
        let decision = harness.owner.set_demand("worker-a", &id, true, sessions, 0);
        if let (None, GrantRefresh::Mint(request)) = (&first, decision) {
            first = Some(request);
        }
    }
    let first = first.expect("the first demand for an open session mints");
    harness.settle(GrantRefresh::Mint(first));
    let request = harness.requests.last().expect("a follow-up is recorded");
    assert_eq!(
        request.session_ids.len(),
        256,
        "the protocol's per-grant maximum"
    );
    assert_eq!(request.session_ids[0], "session-0000");
}
