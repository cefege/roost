//! The fixtures the three direct-terminal grant test binaries share: the
//! session table, the harness that completes a mint, and the one helper that
//! leaves a mint outstanding. `Harness` is the subject of every sibling, so it
//! is defined once here. Ported from `apps/web/tests/localTerminalGrants.test.ts`:
//! the harness performs the mint where v2 mocked a promise, so every hostile
//! case v2 mocked one for is a plain call.

#![allow(dead_code)]

use std::collections::BTreeMap;

use roost_client_core::client::local::grants::GrantOwner;
use roost_client_core::client::local::{
    GrantMintAnswer, GrantMintRequest, GrantRefresh, GrantRefreshReason, GrantSessionFact,
};

/// The session table v2's `rootStore.sessions` mock stands in for.
pub fn sessions() -> BTreeMap<String, GrantSessionFact> {
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
pub struct Harness {
    pub owner: GrantOwner,
    pub requests: Vec<GrantMintRequest>,
    pub sessions: BTreeMap<String, GrantSessionFact>,
    pub now_ms: u64,
}

impl Harness {
    pub fn new() -> Self {
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
    pub fn answer_for(&mut self, request: &GrantMintRequest) -> GrantMintAnswer {
        GrantMintAnswer {
            grant_id: format!(
                "grant-{}-{}",
                request.worker_fp,
                request.session_ids.join("-")
            ),
            secret: format!("secret-{}-{}", request.worker_fp, self.requests.len()),
            ttl_ms: 43_200_000,
            worker_epoch: format!("epoch-{}", request.worker_fp),
            peer_supported: true,
            stun_urls: Vec::new(),
            input_route_supported: true,
        }
    }

    /// Answer a mint, recording it, repeating while an answer owes another.
    pub fn settle(&mut self, decision: GrantRefresh) -> GrantRefresh {
        let mut current = decision;
        for _ in 0..8 {
            let GrantRefresh::Mint(request) = current.clone() else {
                return current;
            };
            self.requests.push(request.clone());
            let answer = self.answer_for(&request);
            let (sessions, now_ms) = (&self.sessions, self.now_ms);
            current = self
                .owner
                .complete_mint(&request, Ok(answer), sessions, now_ms);
        }
        panic!("a refresh chain must terminate");
    }

    pub fn demand(&mut self, worker_fp: &str, session_id: &str, active: bool) -> GrantRefresh {
        let (sessions, now_ms) = (&self.sessions, self.now_ms);
        let decision = self
            .owner
            .set_demand(worker_fp, session_id, active, sessions, now_ms);
        self.settle(decision)
    }

    pub fn renew(&mut self, worker_fp: &str) -> GrantRefresh {
        let (sessions, now_ms) = (&self.sessions, self.now_ms);
        let decision = self
            .owner
            .refresh(worker_fp, GrantRefreshReason::Renewal, sessions, now_ms);
        self.settle(decision)
    }

    /// The session list each mint this worker asked for, in order.
    pub fn requested(&self, worker_fp: &str) -> Vec<Vec<String>> {
        self.requests
            .iter()
            .filter(|request| request.worker_fp == worker_fp)
            .map(|request| request.session_ids.clone())
            .collect()
    }
}

/// Open a mint on `worker-a` and leave it outstanding.
pub fn open_mint(harness: &mut Harness) -> GrantMintRequest {
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
