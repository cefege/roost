//! The direct-terminal grant lifecycle: scope, the fences, and the backoff.
//! Ported from `apps/web/tests/localTerminalGrants.test.ts`, which fences a
//! delayed credential at auth and at retirement. This port drives the same
//! boundary as a value: `refresh` returns the request and the harness performs
//! the mint, so every hostile case v2 mocked a promise for is a plain call.

use std::collections::{BTreeMap, BTreeSet};

use roost_client_core::client::local::grants::{GrantOwner, LOCAL_TERMINAL_GRANT_RETRY_MS};
use roost_client_core::client::local::{
    GrantMintAnswer, GrantMintRequest, GrantRefresh, GrantRefreshReason, GrantSessionFact,
};

/// The session table v2's `rootStore.sessions` mock stands in for.
fn sessions() -> BTreeMap<String, GrantSessionFact> {
    let open = |worker_fp: &str| GrantSessionFact {
        worker_fp: worker_fp.to_string(),
        open: true,
    };
    BTreeMap::from([
        ("session-a".to_string(), open("worker-a")),
        ("session-a2".to_string(), open("worker-a")),
        ("session-b".to_string(), open("worker-b")),
    ])
}

/// One document's grants plus the requests its mints produced. The harness
/// completes a mint, so a test holds one OPEN by leaving the decision unconsumed
/// — what v2's `Promise.withResolvers` did, and the only way to reach the
/// coalescing and fencing cases.
struct Harness {
    owner: GrantOwner,
    requests: Vec<GrantMintRequest>,
    sessions: BTreeMap<String, GrantSessionFact>,
    now_ms: u64,
}

impl Harness {
    fn new() -> Self {
        Self {
            owner: GrantOwner::new("tab-test", "device-test"),
            requests: Vec::new(),
            sessions: sessions(),
            now_ms: 0,
        }
    }

    /// v2's `response`. The secret gains a counter because a real coordinator
    /// mints a fresh one every time: a reused fixture secret would model the
    /// replay the client must refuse rather than a renewal.
    fn answer_for(&mut self, request: &GrantMintRequest) -> GrantMintAnswer {
        GrantMintAnswer {
            grant_id: format!("grant-{}-{}", request.worker_fp, request.session_ids.join("-")),
            secret: format!("secret-{}-{}", request.worker_fp, self.requests.len()),
            ttl_ms: 43_200_000,
            worker_epoch: format!("epoch-{}", request.worker_fp),
            peer_supported: true,
            stun_urls: Vec::new(),
            input_route_supported: true,
        }
    }

    /// Answer a mint, recording it, repeating while an answer owes another.
    fn settle(&mut self, decision: GrantRefresh) -> GrantRefresh {
        let mut current = decision;
        for _ in 0..8 {
            let GrantRefresh::Mint(request) = current.clone() else {
                return current;
            };
            self.requests.push(request.clone());
            let answer = self.answer_for(&request);
            let (sessions, now_ms) = (&self.sessions, self.now_ms);
            current = self.owner.complete_mint(&request, Ok(answer), sessions, now_ms);
        }
        panic!("a refresh chain must terminate");
    }

    fn demand(&mut self, worker_fp: &str, session_id: &str, active: bool) -> GrantRefresh {
        let (sessions, now_ms) = (&self.sessions, self.now_ms);
        let decision =
            self.owner
                .set_demand(worker_fp, session_id, active, sessions, now_ms);
        self.settle(decision)
    }

    fn renew(&mut self, worker_fp: &str) -> GrantRefresh {
        let (sessions, now_ms) = (&self.sessions, self.now_ms);
        let decision =
            self.owner
                .refresh(worker_fp, GrantRefreshReason::Renewal, sessions, now_ms);
        self.settle(decision)
    }

    /// The session list each mint this worker asked for, in order.
    fn requested(&self, worker_fp: &str) -> Vec<Vec<String>> {
        self.requests
            .iter()
            .filter(|request| request.worker_fp == worker_fp)
            .map(|request| request.session_ids.clone())
            .collect()
    }
}

/// Open a mint on `worker-a` and leave it outstanding.
fn open_mint(harness: &mut Harness) -> GrantMintRequest {
    let sessions = &harness.sessions;
    let now_ms = harness.now_ms;
    let decision = harness
        .owner
        .set_demand("worker-a", "session-a", true, sessions, now_ms);
    let GrantRefresh::Mint(request) = decision else {
        panic!("the first demand for an open session must mint");
    };
    harness.requests.push(request.clone());
    request
}

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
    assert_eq!(harness.requested("worker-b"), vec![vec!["session-b".to_string()]]);
    assert!(harness.requests.iter().all(|r| r.tab_id == "tab-test"), "bound to this tab");

    harness.renew("worker-a");
    assert_eq!(harness.requests.len(), 3, "a renewal mints once more");
    assert_eq!(
        harness.owner.current("worker-b").map(|g| g.grant_id.as_str()),
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
    let completed = harness.owner.complete_mint(&first, Ok(answer), sessions, now_ms);
    let GrantRefresh::Mint(follow_up) = completed else {
        panic!("the expanded demand is owed its own mint before it can be used");
    };
    assert_eq!(follow_up.session_ids, vec!["session-a", "session-a2"]);
    harness.requests.push(follow_up.clone());

    let answer = harness.answer_for(&follow_up);
    let sessions = &harness.sessions;
    let installed = harness.owner.complete_mint(&follow_up, Ok(answer), sessions, now_ms);
    let GrantRefresh::Installed(grant) = installed else {
        panic!("the follow-up mint installs");
    };
    assert_eq!(
        grant.session_ids,
        BTreeSet::from(["session-a".to_string(), "session-a2".to_string()]),
        "the installed grant names the expanded set, not the original one"
    );
    let two = vec!["session-a".to_string(), "session-a2".to_string()];
    assert_eq!(harness.requested("worker-a"), vec![vec!["session-a".to_string()], two]);
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
    let standing = harness.owner.current("worker-a").map(|g| g.session_ids.clone());
    let both = BTreeSet::from(["session-a".to_string(), "session-a2".to_string()]);
    assert_eq!(standing, Some(both));
}

#[test]
fn does_not_install_a_grant_minted_before_an_auth_boundary() {
    let mut harness = Harness::new();
    let request = open_mint(&mut harness);
    harness.owner.set_auth_generation(1);
    let answer = harness.answer_for(&request);

    assert_eq!(
        harness
            .owner
            .complete_mint(&request, Ok(answer), &harness.sessions, 0),
        GrantRefresh::Discarded,
        "a credential minted for a replaced identity is never installed"
    );
    assert_eq!(harness.owner.current("worker-a"), None);
}

#[test]
fn removal_fences_an_in_flight_mint_and_a_previously_valid_grant() {
    let mut harness = Harness::new();
    let request = open_mint(&mut harness);
    harness.owner.retire_worker("worker-a");
    let answer = harness.answer_for(&request);

    assert_eq!(
        harness
            .owner
            .complete_mint(&request, Ok(answer), &harness.sessions, 0),
        GrantRefresh::Discarded,
        "a retirement fences an answer that had not arrived yet"
    );
    assert_eq!(harness.owner.current("worker-a"), None);
    let published = harness.owner.take_publications();
    assert_eq!(published.len(), 1, "one publication: the retirement, not the answer");
    assert_eq!(published[0].grant, None, "consumers are told the grant is GONE");

    assert!(harness.owner.is_worker_retired("worker-a"));

    // The other half: a previously VALID grant dies on the same retirement.
    let mut valid = Harness::new();
    valid.demand("worker-b", "session-b", true);
    assert!(valid.owner.current("worker-b").is_some());
    valid.owner.retire_worker("worker-b");
    assert_eq!(valid.owner.current("worker-b"), None);
    let asked = valid.requests.len();
    assert_eq!(
        valid.renew("worker-b"),
        GrantRefresh::Standing(None),
        "a retired worker mints nothing again, and the answer is no grant"
    );
    assert_eq!(valid.requests.len(), asked, "the renewal asked for nothing");
}

#[test]
fn retiring_an_absent_worker_blocks_new_demand_until_auth_reset_while_another_worker_remains_usable() {
    let mut harness = Harness::new();
    harness.owner.retire_worker("worker-a");
    let blocked = harness.demand("worker-a", "session-a", true);
    assert_eq!(
        blocked,
        GrantRefresh::Standing(None),
        "a worker retired before it was ever seen still blocks new demand"
    );
    assert!(harness.requests.is_empty(), "and produces no mint at all");

    harness.demand("worker-b", "session-b", true);
    assert_eq!(
        harness.requested("worker-b"),
        vec![vec!["session-b".to_string()]],
        "losing one worker must not cost another one its terminal"
    );

    harness.owner.reset();
    harness.demand("worker-a", "session-a", true);
    assert_eq!(
        harness.requests.last().map(|r| r.worker_fp.as_str()),
        Some("worker-a"),
        "the auth boundary reset clears the retirement and mints again"
    );
}

#[test]
fn mints_an_initial_grant_without_a_workers_projection() {
    let mut harness = Harness::new();
    // Nothing about `worker-a` is known anywhere: no workers row, no grant, no
    // prior demand. The session table is the only input the mint reads.
    harness.demand("worker-a", "session-a", true);
    assert_eq!(harness.requested("worker-a"), vec![vec!["session-a".to_string()]]);
    let grant = harness.owner.current("worker-a").expect("the mint installed");
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
    let decision = harness.owner.refresh("worker-a", reason, sessions, harness.now_ms);
    harness.settle(decision);
    assert_eq!(harness.requests.len(), 2, "a cleared window mints at once");
    assert_eq!(
        harness.requests[1].session_ids,
        vec!["session-a", "session-a2"],
        "and the retried request still names the exact sessions"
    );
    // Only a REFUSAL arms a backoff, so that successful mint armed nothing.
    harness.demand("worker-a", "session-b", true);
    assert_eq!(harness.requests.len(), 2, "an ungrantable session mints nothing");
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
    assert!(harness.owner.current("worker-a").is_some(), "a grant is back");
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
    assert_eq!(request.session_ids.len(), 256, "the protocol's per-grant maximum");
    assert_eq!(request.session_ids[0], "session-0000");
}
