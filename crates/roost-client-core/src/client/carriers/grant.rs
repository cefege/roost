//! The direct-grant lifecycle: requested, granted, refused, expired, retired.
//! Owned by `client::carriers`, consulted before any transport is opened, and
//! dependent on `Effect::RequestDirectGrant` alone. A carrier with no live
//! grant never authenticates, and a request that RETURNS without a worker's
//! acknowledgement is a REFUSAL rather than a pending state. Ported from
//! `apps/web/src/store/transport/local-terminal-grants.ts`.
//!
//! The credential the lifecycle hands out, and the rule that says whether it
//! still admits a carrier, is `grant::credential`.

mod credential;

pub use credential::DirectGrant;

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
    /// The sessions the grant should name changed: the views' demand and any
    /// pre-warm, as one set. A whole set rather than a session at a time,
    /// because a mint names the whole set (`request`), and growing it one
    /// session per step asks once per session and lets a narrower answer land.
    DemandReplaced {
        /// Every session the grant should name now.
        session_ids: BTreeSet<String>,
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
    /// A mint this lifecycle asked for has not answered yet. Every request is
    /// answered by exactly one `Minted` or `Refused`, so this never strands.
    in_flight: bool,
    /// A mint was owed while one was in flight. v2 coalesces concurrent mints
    /// (`local-terminal-grants.ts` `refreshAgain`): two racing requests let the
    /// narrower answer land LAST, so the client holds a scope the worker has
    /// already replaced and every `Ready` it gets names sessions outside it.
    refresh_again: bool,
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
            in_flight: false,
            refresh_again: false,
            last_detail: None,
        }
    }

    /// Where this worker's grant is.
    pub fn phase(&self) -> GrantPhase {
        self.phase
    }

    /// The sessions the grant should name: the views' demand and any pre-warm.
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
        self.grant
            .as_ref()
            .filter(|grant| !grant.is_expired(now_ms))
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
            GrantInput::DemandReplaced { session_ids } => self.demand_replaced(session_ids),
            GrantInput::Minted(minted) => self.minted(minted),
            GrantInput::Refused { now_ms, reason } => {
                if self.phase == GrantPhase::Retired {
                    return Vec::new();
                }
                // A request that RETURNS is a refusal, not a pending state. No
                // credential is cleared: there was never one to clear, and a
                // refusal says the worker did not acknowledge — not that a live
                // secret stopped working. A coalesced re-ask waits for the
                // retry like any other, as v2's `retryAtMs` gate makes it.
                self.in_flight = false;
                self.refresh_again = false;
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
                self.in_flight = false;
                self.refresh_again = false;
                self.last_detail = Some("worker retired".to_string());
                Vec::new()
            }
        }
    }

    /// The wanted set changed. One that GREW past the scope the last mint
    /// covered is asked for again rather than being quietly stretched; one that
    /// only shrank is not: a mint replaces the worker's whole scope, and the
    /// worker closes every carrier on a grant that lost a session, so
    /// narrowing on its own would cost live routes for nothing.
    fn demand_replaced(&mut self, session_ids: BTreeSet<String>) -> Vec<CarrierEffect> {
        if self.phase == GrantPhase::Retired {
            return Vec::new();
        }
        let grew = !session_ids.is_subset(&self.demanded);
        self.demanded = session_ids;
        if !grew || self.covers_demand() {
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

    /// The answer to the one mint in flight. A demand that grew while it was
    /// out is asked for NOW, in one more mint, before the narrower scope can
    /// open anything: v2's `refresh` re-runs on `refreshAgain` and mints only
    /// when the answer does not cover what is wanted.
    fn minted(&mut self, minted: DirectGrant) -> Vec<CarrierEffect> {
        if self.phase == GrantPhase::Retired {
            return Vec::new();
        }
        if minted.worker_fp != self.worker_fp {
            self.last_detail = Some("a grant for another worker arrived here".to_string());
            return Vec::new();
        }
        self.in_flight = false;
        self.retry_at_ms = 0;
        self.last_detail = None;
        self.phase = GrantPhase::Granted;
        self.grant = Some(minted);
        if std::mem::take(&mut self.refresh_again) && !self.covers_demand() {
            return self.request();
        }
        Vec::new()
    }

    /// Whether the held credential names every session a view wants here.
    fn covers_demand(&self) -> bool {
        self.grant.as_ref().is_some_and(|grant| {
            self.demanded
                .iter()
                .all(|id| grant.session_ids.contains(id))
        })
    }

    /// A fault named the credential itself: drop it and ask for a fresh mint
    /// NOW, as v2's `dropTerminalGrant` does (`local-terminal-grants.ts:154-160`
    /// clears `retryAtMs`, and the next `maybeStart` refreshes with no wait,
    /// `terminal-peer.ts:198`). The bounded retry belongs to a mint that was
    /// REFUSED, not to a secret the worker stopped honouring.
    fn revoked(&mut self, fault: CarrierFault, now_ms: u64) -> Vec<CarrierEffect> {
        if self.phase == GrantPhase::Retired {
            return Vec::new();
        }
        self.last_detail = Some(fault.as_str().to_string());
        if fault.keeps_grant() {
            return Vec::new();
        }
        self.grant = None;
        self.retry_at_ms = 0;
        self.phase = if fault == CarrierFault::GrantExpired {
            GrantPhase::Expired
        } else {
            GrantPhase::Unavailable
        };
        tracing::info!(
            target: "carriers",
            worker_fp = %self.worker_fp,
            fault = fault.as_str(),
            now_ms,
            "direct grant dropped; a fresh mint is asked for any demand"
        );
        self.request()
    }

    /// Move to `Requested` and ask for every demanded session in one mint.
    ///
    /// ONE request naming the whole set, as v2's `refresh` mints `eligible`
    /// (`local-terminal-grants.ts`): the coordinator installs exactly the set a
    /// mint names under the tab's one grant id, so a per-session request would
    /// re-install the grant narrowed to that one session and strip every other
    /// session from a carrier that is already serving it.
    fn request(&mut self) -> Vec<CarrierEffect> {
        if self.demanded.is_empty() {
            return Vec::new();
        }
        self.phase = GrantPhase::Requested;
        self.retry_at_ms = 0;
        if self.in_flight {
            self.refresh_again = true;
            return Vec::new();
        }
        self.in_flight = true;
        vec![CarrierEffect::Core(Effect::RequestDirectGrant {
            session_ids: self.demanded.iter().cloned().collect(),
            worker_fp: self.worker_fp.clone(),
        })]
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
