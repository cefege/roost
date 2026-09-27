//! The direct-grant lifecycle: requested, granted, refused, expired, retired.
//! Owned by `client::carriers`, consulted before any transport is opened, and
//! dependent on `Effect::RequestDirectGrant` alone. A carrier with no live
//! grant never authenticates, and a request that RETURNS without a worker's
//! acknowledgement is a REFUSAL rather than a pending state. Ported from
//! `apps/web/src/store/transport/local-terminal-grants.ts`.

use std::collections::BTreeSet;

use crate::client::carriers::{CarrierEffect, CarrierFault};
use crate::effect::Effect;
use crate::terminal::token::TerminalTransport;

/// How long to wait before asking for a grant again after a refusal. Long
/// enough that a coordinator blip is not a request storm, short enough that a
/// worker that has just come back is reached.
pub const GRANT_RETRY_MS: u64 = 30_000;

/// How long a live grant is asked to be renewed before it expires.
pub const GRANT_RENEW_MS: u64 = 60 * 60_000;

/// One coordinator-authorized, memory-only, worker-acknowledged credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectGrant {
    /// The coordinator's id for this grant.
    pub grant_id: String,
    /// The secret. It exists only in this document's memory, which is why it is
    /// never written, never logged, and never part of a connection id.
    pub secret: String,
    /// The worker whose loopback door or peer this grant opens.
    pub worker_fp: String,
    /// The worker PROCESS the grant was minted against. Empty means the worker
    /// reported none, and a grant without one cannot open a peer, because the
    /// offer is bound to a process epoch.
    pub worker_epoch: String,
    /// The tab the grant names.
    pub tab_id: String,
    /// The device the grant names.
    pub device_fingerprint: String,
    /// The exact sessions the grant admits. Never widened after admission.
    pub session_ids: BTreeSet<String>,
    /// Whether the worker offers the WebRTC peer carrier at all.
    pub peer_supported: bool,
    /// Whether the worker implements `terminal-input-route-v1`.
    pub input_route_supported: bool,
    /// Opportunistic address discovery only. Empty disables it, and it is never
    /// a relay guarantee.
    pub stun_urls: Vec<String>,
    /// The host's clock value at which this grant is dead. Zero is refused: an
    /// unbounded credential is not a time-bounded one.
    pub expires_at_ms: u64,
}

impl DirectGrant {
    /// Whether the grant's own deadline has passed.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_at_ms != 0 && now_ms >= self.expires_at_ms
    }

    /// Whether this grant can open a carrier for `transport`.
    ///
    /// The peer path additionally requires a worker process epoch, because the
    /// coordinator binds the offer to one
    /// (`protocol/spec/direct-terminal.md:25`).
    pub fn admits(&self, transport: TerminalTransport) -> bool {
        if self.grant_id.is_empty() || self.secret.is_empty() || self.worker_fp.is_empty() {
            return false;
        }
        match transport {
            TerminalTransport::Loopback => true,
            TerminalTransport::Peer => self.peer_supported && !self.worker_epoch.is_empty(),
            TerminalTransport::Sync => false,
        }
    }
}

/// Where one worker's grant is in its lifecycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GrantPhase {
    /// No credential, and none wanted.
    #[default]
    Absent,
    /// A request is outstanding. A carrier MUST NOT authenticate on this, and
    /// there is deliberately no path that opens a transport from it: the
    /// request's return value is the whole answer.
    Requested,
    /// A live credential, and the only phase that opens a transport.
    Granted,
    /// The request returned and the worker did not acknowledge, or a fault named
    /// the credential itself. A refusal, not a wait.
    Unavailable,
    /// The deadline passed. Distinct from `Unavailable` because the credential
    /// was real once, and the worker process it named may be gone.
    Expired,
    /// The worker was removed. Terminal: demand is never re-armed for it.
    Retired,
}

/// One thing that happened to a worker's grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantInput {
    /// A view started wanting a session on this worker.
    DemandAdded {
        /// The session.
        session_id: String,
    },
    /// A view stopped wanting a session on this worker.
    DemandRemoved {
        /// The session.
        session_id: String,
    },
    /// The coordinator minted a credential and the worker acknowledged it.
    Minted(DirectGrant),
    /// The request returned without a worker's acknowledgement. A REFUSAL.
    Refused {
        /// The host's clock, so the retry is an instant rather than a duration.
        now_ms: u64,
        /// The host's own detail, never the value that failed to match.
        reason: String,
    },
    /// A fault named this grant. Only the faults that name the CREDENTIAL clear
    /// it; the rest leave it in place.
    Revoked {
        /// Why it was revoked.
        fault: CarrierFault,
        /// The host's clock.
        now_ms: u64,
    },
    /// The host's clock reached the renewal point.
    RenewDue {
        /// The host's clock.
        now_ms: u64,
    },
    /// The host's clock reached the retry point.
    RetryDue {
        /// The host's clock.
        now_ms: u64,
    },
    /// The worker was removed.
    WorkerRetired,
}

/// What one pass over a grant's own deadlines found due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantSweep {
    /// Nothing was due.
    Nothing,
    /// The deadline passed, so the credential is gone.
    Expired,
    /// The renewal point was reached, so a fresh mint is owed.
    RenewDue,
}

/// One worker's grant state, and the requests it owes.
#[derive(Debug, Clone)]
pub struct GrantLifecycle {
    worker_fp: String,
    demanded: BTreeSet<String>,
    phase: GrantPhase,
    grant: Option<DirectGrant>,
    retry_at_ms: u64,
    /// When the live grant was minted, which is what the renewal is measured
    /// from. The grant's own deadline is the authority on when it DIES; this is
    /// only when to ask for the next one.
    pub(crate) refresh_at_ms: Option<u64>,
    last_detail: Option<String>,
}

impl GrantLifecycle {
    /// A lifecycle for one worker, with no credential and no demand.
    pub fn new(worker_fp: impl Into<String>) -> Self {
        Self {
            worker_fp: worker_fp.into(),
            demanded: BTreeSet::new(),
            phase: GrantPhase::Absent,
            grant: None,
            retry_at_ms: 0,
            refresh_at_ms: None,
            last_detail: None,
        }
    }

    /// Where this worker's grant is.
    pub fn phase(&self) -> GrantPhase {
        self.phase
    }

    /// The sessions a view currently wants on this worker.
    pub fn demanded_sessions(&self) -> &BTreeSet<String> {
        &self.demanded
    }

    /// The last refusal or revocation detail, for a host's log.
    pub fn last_detail(&self) -> Option<&str> {
        self.last_detail.as_deref()
    }

    /// The live credential, and only ever a live one.
    ///
    /// This is the single gate a carrier passes through, and it answers `None`
    /// for every phase except `Granted` — including a credential whose deadline
    /// has passed, because a carrier that authenticates on a dead grant is one
    /// the worker will refuse for a reason the client already knew.
    pub fn live_grant(&self, now_ms: u64) -> Option<&DirectGrant> {
        if self.phase != GrantPhase::Granted {
            return None;
        }
        self.grant.as_ref().filter(|grant| !grant.is_expired(now_ms))
    }

    /// Whether a live grant exists for `transport` right now.
    pub fn admits(&self, transport: TerminalTransport, now_ms: u64) -> bool {
        self.live_grant(now_ms)
            .is_some_and(|grant| grant.admits(transport))
    }

    /// One pass over this grant's own deadlines. Expiry is decided HERE, from
    /// the client's clock, rather than left for a worker to announce: a
    /// credential this client knows is dead is not one to authenticate on, and
    /// waiting to be told would keep a dead secret in memory for a whole grant
    /// lifetime. Expiry is checked first because a credential past its deadline
    /// has nothing left to renew.
    pub fn sweep(&mut self, now_ms: u64) -> GrantSweep {
        if self.phase != GrantPhase::Granted {
            return GrantSweep::Nothing;
        }
        if self.live_grant(now_ms).is_none() {
            self.phase = GrantPhase::Expired;
            self.grant = None;
            return GrantSweep::Expired;
        }
        let minted = self.refresh_at_ms.unwrap_or(now_ms);
        if now_ms.saturating_sub(minted) >= GRANT_RENEW_MS && !self.demanded.is_empty() {
            return GrantSweep::RenewDue;
        }
        GrantSweep::Nothing
    }

    /// Fold one grant event in, and return the requests it owes.
    pub fn step(&mut self, input: GrantInput) -> Vec<CarrierEffect> {
        match input {
            GrantInput::DemandAdded { session_id } => self.demand_added(session_id),
            GrantInput::DemandRemoved { session_id } => {
                self.demanded.remove(&session_id);
                Vec::new()
            }
            GrantInput::Minted(minted) => self.minted(minted),
            GrantInput::Refused { now_ms, reason } => {
                if self.phase == GrantPhase::Retired {
                    return Vec::new();
                }
                // A request that RETURNS is a refusal, not a pending state. No
                // credential is cleared: there was never one to clear, and a
                // refusal says the worker did not acknowledge — not that a live
                // secret stopped working.
                self.phase = GrantPhase::Unavailable;
                self.last_detail = Some(reason);
                self.arm_retry(now_ms)
            }
            GrantInput::Revoked { fault, now_ms } => self.revoked(fault, now_ms),
            GrantInput::RenewDue { now_ms } => {
                if self.phase != GrantPhase::Granted
                    || self.live_grant(now_ms).is_none()
                    || self.demanded.is_empty()
                {
                    return Vec::new();
                }
                self.request()
            }
            GrantInput::RetryDue { now_ms } => {
                if !matches!(self.phase, GrantPhase::Unavailable | GrantPhase::Expired) {
                    return Vec::new();
                }
                if self.retry_at_ms == 0 || self.retry_at_ms > now_ms {
                    return Vec::new();
                }
                self.request()
            }
            GrantInput::WorkerRetired => {
                self.phase = GrantPhase::Retired;
                self.grant = None;
                self.demanded.clear();
                self.retry_at_ms = 0;
                self.last_detail = Some("worker retired".to_string());
                Vec::new()
            }
        }
    }

    /// A new session is outside the scope the last mint covered, so the grant
    /// is asked for again rather than being quietly stretched.
    fn demand_added(&mut self, session_id: String) -> Vec<CarrierEffect> {
        if self.phase == GrantPhase::Retired {
            return Vec::new();
        }
        if !self.demanded.insert(session_id) {
            return Vec::new();
        }
        let covered = self
            .grant
            .as_ref()
            .is_some_and(|grant| self.demanded.iter().all(|id| grant.session_ids.contains(id)));
        if covered {
            return Vec::new();
        }
        if self.phase == GrantPhase::Unavailable || self.phase == GrantPhase::Expired {
            // A retry is already armed. Re-asking now would turn one refusal
            // into a request per demand change.
            if self.retry_at_ms != 0 {
                return Vec::new();
            }
        }
        self.request()
    }

    fn minted(&mut self, minted: DirectGrant) -> Vec<CarrierEffect> {
        if self.phase == GrantPhase::Retired {
            return Vec::new();
        }
        if minted.worker_fp != self.worker_fp {
            self.last_detail = Some("a grant for another worker arrived here".to_string());
            return Vec::new();
        }
        self.retry_at_ms = 0;
        self.last_detail = None;
        self.phase = GrantPhase::Granted;
        self.grant = Some(minted);
        Vec::new()
    }

    fn revoked(&mut self, fault: CarrierFault, now_ms: u64) -> Vec<CarrierEffect> {
        if self.phase == GrantPhase::Retired {
            return Vec::new();
        }
        self.last_detail = Some(fault.as_str().to_string());
        if fault.keeps_grant() {
            return Vec::new();
        }
        self.grant = None;
        self.phase = if fault == CarrierFault::GrantExpired {
            GrantPhase::Expired
        } else {
            GrantPhase::Unavailable
        };
        if self.demanded.is_empty() {
            return Vec::new();
        }
        self.arm_retry(now_ms)
    }

    /// Move to `Requested` and ask for every demanded session.
    ///
    /// One request per session, in the core's own vocabulary. The coordinator
    /// unions a tab's requests into one grant scope
    /// (`protocol/spec/direct-terminal.md:23`), so the client does not invent a
    /// batch form `Effect::RequestDirectGrant` does not have.
    fn request(&mut self) -> Vec<CarrierEffect> {
        if self.demanded.is_empty() {
            return Vec::new();
        }
        self.phase = GrantPhase::Requested;
        self.retry_at_ms = 0;
        self.demanded
            .iter()
            .map(|session_id| {
                CarrierEffect::Core(Effect::RequestDirectGrant {
                    session_id: session_id.clone(),
                    worker_fp: self.worker_fp.clone(),
                })
            })
            .collect()
    }

    /// Arm the retry. The core owns no timer, so this reports the INSTANT rather
    /// than a delay: a host that cannot schedule one leaves the session on Sync
    /// instead of spinning.
    fn arm_retry(&mut self, now_ms: u64) -> Vec<CarrierEffect> {
        self.retry_at_ms = now_ms.saturating_add(GRANT_RETRY_MS);
        vec![CarrierEffect::RetryAt {
            at_ms: self.retry_at_ms,
        }]
    }
}
