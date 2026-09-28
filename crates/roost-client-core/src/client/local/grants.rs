//! The coordinator-minted direct-terminal grant for one worker, and the
//! lifecycle that keeps it current. Asked before any loopback socket opens, and
//! driven whenever demand or the auth generation moves.
//!
//! Three properties are the whole module, and each has a test that fails if it
//! is weakened. A grant NAMES exact sessions — one worker, one tab, one device,
//! and the session list the coordinator acknowledged, never "everything". It is
//! BOUNDED in time, by the coordinator's own TTL read against a `now_ms` the
//! caller passes. And a secret holds ONE live socket, which
//! `door::SecretUseLedger` enforces because the worker's door answers a replay
//! by REPLACING the socket it already gave out.
//!
//! Ported from `apps/web/src/store/transport/local-terminal-grants.ts`. The
//! request and the decision to send it are `outbound`'s vocabulary, and the mint
//! is NOT performed here, so every rule below is provable with no coordinator
//! and no clock.

use std::collections::{BTreeMap, BTreeSet};

use roost_protocol::terminal_peer::peer::TERMINAL_PEER_MAX_SESSIONS_PER_GRANT;

use crate::client::local::{
    GrantMintAnswer, GrantMintRequest, GrantPublication, GrantRefresh, GrantRefreshReason,
    GrantSessionLookup, LocalTerminalGrant,
};

/// How often a grant that still has demand is re-minted. Well inside the
/// coordinator's own lifetime, so a renewal lands before the current one ends.
pub const LOCAL_TERMINAL_GRANT_RENEW_MS: u64 = 60 * 60_000;

/// How long a REFUSED mint is left alone. A mint that succeeded arms nothing,
/// so a coordinator that is merely slow does not throttle its own successes.
pub const LOCAL_TERMINAL_GRANT_RETRY_MS: u64 = 30_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WorkerGrantState {
    wanted: BTreeSet<String>,
    grant: Option<LocalTerminalGrant>,
    retry_at_ms: u64,
    demand_version: u64,
    refresh_again: bool,
    in_flight: Option<GrantMintRequest>,
    captured_demand_version: u64,
    captured_auth_generation: u64,
}

/// One document's direct-terminal grants, keyed by worker.
///
/// Keyed by worker so a loopback attempt and a peer attempt cannot replace each
/// other's credentials, and fenced on the auth generation so a credential minted
/// for an identity that has since been replaced never reaches a socket. The
/// renewal timer is the HOST's: this owns the list it walks.
#[derive(Debug, Clone)]
pub struct GrantOwner {
    states: BTreeMap<String, WorkerGrantState>,
    retired: BTreeSet<String>,
    tab_id: String,
    device_fingerprint: String,
    auth_generation: u64,
    publications: Vec<GrantPublication>,
}

impl GrantOwner {
    /// An owner for one tab on one device.
    pub fn new(tab_id: impl Into<String>, device_fingerprint: impl Into<String>) -> Self {
        Self {
            states: BTreeMap::new(),
            retired: BTreeSet::new(),
            tab_id: tab_id.into(),
            device_fingerprint: device_fingerprint.into(),
            auth_generation: 0,
            publications: Vec::new(),
        }
    }

    /// Record a new auth generation. Anything minted under the previous one is
    /// refused when it arrives.
    pub fn set_auth_generation(&mut self, generation: u64) {
        self.auth_generation = generation;
    }

    /// This worker's current grant.
    pub fn current(&self, worker_fp: &str) -> Option<&LocalTerminalGrant> {
        self.states.get(worker_fp)?.grant.as_ref()
    }

    /// Whether the coordinator confirmed this worker is gone, so nothing mints
    /// for it until the auth boundary resets.
    pub fn is_worker_retired(&self, worker_fp: &str) -> bool {
        self.retired.contains(worker_fp)
    }

    /// Record that a view wants a session, or that it stopped wanting it.
    ///
    /// Removing demand leaves a valid grant intact until normal renewal: the
    /// worker holds a PTY open on the strength of that grant, and revoking it
    /// under a live view would strand a shell. What removal stops is GROWTH —
    /// without fresh demand there is no second mint.
    pub fn set_demand(
        &mut self,
        worker_fp: &str,
        session_id: &str,
        active: bool,
        sessions: &dyn GrantSessionLookup,
        now_ms: u64,
    ) -> GrantRefresh {
        if worker_fp.is_empty() || session_id.is_empty() || self.retired.contains(worker_fp) {
            return self.standing(worker_fp);
        }
        let state = self.state_mut(worker_fp);
        if active {
            if !state.wanted.insert(session_id.to_string()) {
                return self.standing(worker_fp);
            }
            state.demand_version += 1;
            return self.refresh(worker_fp, GrantRefreshReason::DemandAdded, sessions, now_ms);
        }
        if !state.wanted.remove(session_id) {
            return self.standing(worker_fp);
        }
        state.demand_version += 1;
        self.standing(worker_fp)
    }

    /// Decide whether a mint is owed, and for exactly which sessions.
    pub fn refresh(
        &mut self,
        worker_fp: &str,
        reason: GrantRefreshReason,
        sessions: &dyn GrantSessionLookup,
        now_ms: u64,
    ) -> GrantRefresh {
        if self.retired.contains(worker_fp) || !self.states.contains_key(worker_fp) {
            return self.standing(worker_fp);
        }
        if self.states[worker_fp].in_flight.is_some() {
            // A second mint while one is open is not a second mint: it is a note
            // that the answer in flight will not cover what is being asked now.
            self.state_mut(worker_fp).refresh_again = true;
            return self.standing(worker_fp);
        }
        let Some(request) = self.mint_request(worker_fp, reason, sessions, now_ms) else {
            return self.standing(worker_fp);
        };
        let auth_generation = self.auth_generation;
        let state = self.state_mut(worker_fp);
        state.captured_demand_version = state.demand_version;
        state.captured_auth_generation = auth_generation;
        state.in_flight = Some(request.clone());
        GrantRefresh::Mint(request)
    }

    /// Report the answer to a mint this owner asked for, fenced.
    ///
    /// The fence is the point: an answer is installed only if it answers the mint
    /// actually in flight, under the auth generation that mint started on, for a
    /// worker not retired since. `Err` is the ORDINARY case — a coordinator that
    /// is down returns nothing, and that is not a state a client must survive.
    pub fn complete_mint(
        &mut self,
        request: &GrantMintRequest,
        outcome: Result<GrantMintAnswer, String>,
        sessions: &dyn GrantSessionLookup,
        now_ms: u64,
    ) -> GrantRefresh {
        let answered = self
            .states
            .get(&request.worker_fp)
            .is_some_and(|state| state.in_flight.as_ref() == Some(request));
        if !answered {
            return GrantRefresh::Discarded;
        }
        let (captured_demand, captured_auth) = {
            let state = self.state_mut(&request.worker_fp);
            let captured = (
                state.captured_demand_version,
                state.captured_auth_generation,
            );
            state.in_flight = None;
            captured
        };
        if self.retired.contains(&request.worker_fp) || self.auth_generation != captured_auth {
            return GrantRefresh::Discarded;
        }
        let installed = self.install(request, outcome, now_ms);
        self.follow_up(
            &request.worker_fp,
            captured_demand,
            installed,
            sessions,
            now_ms,
        )
    }

    /// The worker rejected a live carrier's credential. Only a fresh mint can
    /// recover from it, so the standing grant goes and the backoff with it.
    pub fn drop(&mut self, worker_fp: &str) {
        let Some(state) = self.states.get_mut(worker_fp) else {
            return;
        };
        if state.grant.is_none() {
            return;
        }
        state.grant = None;
        state.retry_at_ms = 0;
        self.publish(worker_fp, None);
    }

    /// Retire a worker the coordinator confirmed is gone: fence the in-flight
    /// mint, clear the standing grant, and block new demand until the auth
    /// boundary resets. Other workers are untouched.
    pub fn retire_worker(&mut self, worker_fp: &str) {
        self.retired.insert(worker_fp.to_string());
        if self.states.remove(worker_fp).is_some() {
            self.publish(worker_fp, None);
        }
    }

    /// Clear one worker's mint backoff, or every worker's. Called when Sync comes
    /// back: what the coordinator refused while it was down is worth re-asking.
    pub fn clear_retry(&mut self, worker_fp: Option<&str>) {
        match worker_fp {
            Some(worker_fp) => {
                if let Some(state) = self.states.get_mut(worker_fp) {
                    state.retry_at_ms = 0;
                }
            }
            None => self.states.values_mut().for_each(|s| s.retry_at_ms = 0),
        }
    }

    /// Revoke every document-held credential. Another authenticated identity may
    /// install its own the moment this returns, and one left behind would be
    /// presented to a worker on its behalf.
    pub fn reset(&mut self) {
        let workers: Vec<String> = self.states.keys().cloned().collect();
        self.states.clear();
        self.retired.clear();
        workers.iter().for_each(|fp| self.publish(fp, None));
    }

    /// Take the publications a host has not read yet.
    pub fn take_publications(&mut self) -> Vec<GrantPublication> {
        std::mem::take(&mut self.publications)
    }

    /// The workers a renewal pass should refresh: every worker with demand.
    pub fn workers_with_demand(&self) -> Vec<String> {
        self.states
            .iter()
            .filter(|(_, state)| !state.wanted.is_empty())
            .map(|(worker_fp, _)| worker_fp.clone())
            .collect()
    }

    /// The request a refresh would send, or `None` when it sends nothing.
    ///
    /// Every arm below is a way a grant would be minted for nothing, or not
    /// minted while the user watches a terminal that cannot work: no demand,
    /// nothing grantable, a demand change already covered, and a refusal inside
    /// its own backoff.
    fn mint_request(
        &self,
        worker_fp: &str,
        reason: GrantRefreshReason,
        sessions: &dyn GrantSessionLookup,
        now_ms: u64,
    ) -> Option<GrantMintRequest> {
        let state = self.states.get(worker_fp)?;
        if state.wanted.is_empty() {
            return None;
        }
        let retained = state
            .grant
            .as_ref()
            .map(|grant| grant.session_ids.clone())
            .unwrap_or_default();
        let eligible = grantable_sessions(worker_fp, &retained, &state.wanted, sessions);
        if eligible.is_empty() {
            return None;
        }
        let covers_all = state
            .grant
            .as_ref()
            .is_some_and(|grant| eligible.is_subset(&grant.session_ids));
        if reason == GrantRefreshReason::DemandAdded && covers_all {
            return None;
        }
        if !covers_all && now_ms < state.retry_at_ms {
            return None;
        }
        Some(GrantMintRequest {
            worker_fp: worker_fp.to_string(),
            session_ids: eligible.iter().cloned().collect(),
            tab_id: self.tab_id.clone(),
        })
    }

    /// Put an answer where it belongs, or arm the retry a refusal earns.
    fn install(
        &mut self,
        request: &GrantMintRequest,
        outcome: Result<GrantMintAnswer, String>,
        now_ms: u64,
    ) -> GrantRefresh {
        let worker_fp = request.worker_fp.clone();
        let minted = outcome.ok().and_then(|answer| {
            LocalTerminalGrant::from_answer(
                answer,
                worker_fp.clone(),
                request.session_ids.iter().cloned().collect(),
                self.tab_id.clone(),
                self.device_fingerprint.clone(),
                now_ms,
            )
        });
        let Some(minted) = minted else {
            self.state_mut(&worker_fp).retry_at_ms =
                now_ms.saturating_add(LOCAL_TERMINAL_GRANT_RETRY_MS);
            return self.standing(&worker_fp);
        };
        let state = self.state_mut(&worker_fp);
        state.retry_at_ms = 0;
        state.grant = Some(minted.clone());
        self.publish(&worker_fp, Some(minted.clone()));
        GrantRefresh::Installed(minted)
    }

    /// Mint again if the demand moved while this answer was in flight.
    fn follow_up(
        &mut self,
        worker_fp: &str,
        captured_demand_version: u64,
        outcome: GrantRefresh,
        sessions: &dyn GrantSessionLookup,
        now_ms: u64,
    ) -> GrantRefresh {
        let Some(state) = self.states.get_mut(worker_fp) else {
            return outcome;
        };
        let moved = state.refresh_again || state.demand_version != captured_demand_version;
        state.refresh_again = false;
        if !moved || state.wanted.is_empty() {
            return outcome;
        }
        self.refresh(worker_fp, GrantRefreshReason::DemandAdded, sessions, now_ms)
    }

    fn standing(&self, worker_fp: &str) -> GrantRefresh {
        GrantRefresh::Standing(
            self.states
                .get(worker_fp)
                .and_then(|state| state.grant.clone()),
        )
    }

    fn publish(&mut self, worker_fp: &str, grant: Option<LocalTerminalGrant>) {
        self.publications.push(GrantPublication {
            worker_fp: worker_fp.to_string(),
            grant,
        });
    }

    fn state_mut(&mut self, worker_fp: &str) -> &mut WorkerGrantState {
        self.states.entry(worker_fp.to_string()).or_default()
    }
}

/// The sessions a mint may name, in the order the cap is applied.
///
/// The sessions the standing grant already holds come FIRST, so a grant that
/// must grow keeps what it has — dropping one mid-life would strand a PTY the
/// worker is already serving. Closed sessions and other workers' sessions are
/// skipped, and the list is capped at the protocol's per-grant maximum.
fn grantable_sessions(
    worker_fp: &str,
    retained: &BTreeSet<String>,
    wanted: &BTreeSet<String>,
    sessions: &dyn GrantSessionLookup,
) -> BTreeSet<String> {
    let mut granted: BTreeSet<String> = BTreeSet::new();
    for session_id in retained.iter().chain(wanted.iter()) {
        let Some(fact) = sessions.grant_session(session_id) else {
            continue;
        };
        if !fact.open || fact.worker_fp != worker_fp {
            continue;
        }
        granted.insert(session_id.clone());
        if granted.len() >= TERMINAL_PEER_MAX_SESSIONS_PER_GRANT {
            break;
        }
    }
    granted
}
