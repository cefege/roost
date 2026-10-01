//! The peer signalling state machine: one worker, one WebRTC attempt at a time,
//! and the order a direct carrier may come up in. Every transition that can
//! change an attempt lives here; what a host may READ about one is projected by
//! `super::signaling_snapshot`, the `PeerSignalling` lane it is driven through.
//! Never a socket, never a timer, and never the promotion, which
//! `RouteRegistry` owns. Ported from
//! `apps/web/src/store/transport/terminal-peer.ts:193-283`; the one addition is
//! fault CLASSIFICATION, which v2 collapses into one `network_failed`.

use std::collections::BTreeSet;

use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT, TERMINAL_PEER_NEGOTIATION_DEADLINE_MS,
};

use crate::client::carriers::faults::{
    FaultState, answer_fault, classify_worker_reason, ready_fault, sdp_is_usable,
};
use crate::client::carriers::grant::{GrantInput, GrantLifecycle, GrantSweep};
use crate::client::carriers::loopback::LoopbackProbe;
use crate::client::carriers::signaling_snapshot::PeerTelemetry;
use crate::client::carriers::{
    CarrierEffect, CarrierEnvironment, CarrierFault, PeerAnswer, PeerAttempt, PeerPhase,
    ReadyTuple, SignallingInput,
};
use crate::terminal::token::TerminalTransport;

/// One worker's WebRTC attempt, and the election around it.
#[derive(Debug)]
pub struct Signalling {
    pub(crate) worker_fp: String,
    pub(crate) demand: BTreeSet<String>,
    pub(crate) active_views: u64,
    pub(crate) env: CarrierEnvironment,
    pub(crate) grant: GrantLifecycle,
    pub(crate) loopback: LoopbackProbe,
    pub(crate) faults: FaultState,
    pub(crate) phase: PeerPhase,
    pub(crate) attempt: Option<PeerAttempt>,
    /// What the transport last measured. Cleared when the attempt it belongs
    /// to ends, so a reader never sees a dead peer's round trip on a route
    /// that has fallen back to Sync.
    pub(crate) telemetry: PeerTelemetry,
    pub(crate) attempt_started_ms: u64,
    /// The authenticated peer held for this worker. Always a PEER: a loopback
    /// carrier is the loopback slice's own connection.
    pub(crate) peer_held: bool,
    pub(crate) now_ms: u64,
    pub(crate) next_attempt_id: u64,
    pub(crate) retired: bool,
}

impl Signalling {
    /// A machine for one worker, with nothing demanded and nothing in flight.
    pub fn new(worker_fp: impl Into<String>, env: CarrierEnvironment, now_ms: u64) -> Self {
        let worker_fp = worker_fp.into();
        Self {
            loopback: LoopbackProbe::new(worker_fp.clone()),
            grant: GrantLifecycle::new(worker_fp.clone()),
            worker_fp,
            demand: BTreeSet::new(),
            active_views: 0,
            env,
            faults: FaultState::default(),
            phase: PeerPhase::Idle,
            attempt: None,
            attempt_started_ms: 0,
            peer_held: false,
            now_ms,
            next_attempt_id: 0,
            telemetry: PeerTelemetry::default(),
            retired: false,
        }
    }

    /// The credential a host would spend to open this worker's carrier now.
    ///
    /// Deliberately NOT reachable from [`PeerAttempt`], which is traced and
    /// `Debug`-printed on every transition: the secret belongs to the moment a
    /// carrier authenticates, not to the description of an attempt that is
    /// still being negotiated.
    pub(crate) fn live_grant(&self, now_ms: u64) -> Option<&crate::client::carriers::DirectGrant> {
        self.grant.live_grant(now_ms)
    }

    /// Fold one observation in, and return what the host should do about it.
    pub fn step(&mut self, input: SignallingInput) -> Vec<CarrierEffect> {
        if self.retired {
            return Vec::new();
        }
        let mut out = Vec::new();
        match input {
            SignallingInput::WorkerRetired => out.extend(self.retire()),
            SignallingInput::Demand {
                session_id, active, ..
            } => out.extend(self.demand(session_id, active)),
            SignallingInput::Grant(grant) => out.extend(self.grant_step(grant)),
            SignallingInput::LocalDoorAnswered { worker_fp } => {
                self.loopback.answered(&worker_fp);
                out.extend(self.start(self.now_ms));
            }
            SignallingInput::LoopbackCarrierStaged { staged } => {
                self.loopback.set_staged(staged);
                if !staged {
                    out.extend(self.start(self.now_ms))
                }
            }
            SignallingInput::OfferReady {
                attempt_id,
                offer_sdp,
            } => out.extend(self.offer(attempt_id, offer_sdp)),
            SignallingInput::AnswerReceived { attempt_id, answer } => {
                out.extend(self.answer(attempt_id, answer))
            }

            SignallingInput::AttemptRefused { attempt_id, reason } => {
                out.extend(self.refused(attempt_id, &reason))
            }
            SignallingInput::PeerAuthenticated { attempt_id, ready } => {
                out.extend(self.authenticated(attempt_id, ready))
            }
            SignallingInput::IceFailed { attempt_id } => {
                out.extend(self.fault(attempt_id, CarrierFault::IceFailed, "peer ICE failed"))
            }
            SignallingInput::ProbeMissed { attempt_id } => {
                out.extend(self.fault(attempt_id, CarrierFault::IceFailed, "heartbeat missed"))
            }
            SignallingInput::PromotionCommitted { token, .. } => {
                if self.holds(&token) {
                    self.faults.cleared();
                    self.set_phase(PeerPhase::Active, None);
                }
            }
            SignallingInput::RetryDue { now_ms } => {
                self.now_ms = now_ms;
                out.extend(self.grant.step(GrantInput::RetryDue { now_ms }));
                out.extend(self.start(now_ms))
            }
            SignallingInput::Sweep { now_ms } => out.extend(self.sweep(now_ms)),
        }
        out
    }

    /// The attempt-may-start gate, in v2's order. The loopback question comes
    /// FIRST, before whether this document can do WebRTC at all: a page on the
    /// worker's own machine must not allocate a peer.
    fn start(&mut self, now_ms: u64) -> Vec<CarrierEffect> {
        if self.active_views == 0 || self.peer_held || self.retired || self.attempt.is_some() {
            return Vec::new();
        }
        if !self.loopback.permits_peer() {
            // `Some` means the probe has not answered, so come back in a grace
            // period; `None` means it answered SAME HOST and the loopback slice
            // owns the carrier. The grace is NOT the fault retry: a grant that
            // arrives, or a door that answers, must be acted on at once.
            //
            self.park_keeping_terminal_reason(PeerPhase::Idle);
            return match self.loopback.recheck_after_ms() {
                Some(wait) => vec![CarrierEffect::RetryAt {
                    at_ms: now_ms.saturating_add(wait),
                }],
                None => Vec::new(),
            };
        }
        if self.faults.retry_at_ms > now_ms {
            return Vec::new();
        }
        if !self.env.peer_transport_available {
            let fault = CarrierFault::Unsupported;
            self.set_phase(PeerPhase::Disabled, Some(fault.reason()));
            return self.report(fault, "this document cannot do WebRTC");
        }
        if self.grant.live_grant(now_ms).is_none() {
            // Parking here is not a stall: the grant lifecycle armed its own
            // retry the moment it last refused.
            self.park_keeping_terminal_reason(PeerPhase::AwaitingGrant);
            return Vec::new();
        }
        if self.faults.hold_down_until_ms > now_ms {
            return self.faults.schedule(self.faults.hold_down_until_ms);
        }
        if self.env.peers_allocated >= TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT as u32 {
            let detail = "this document already holds all of its peers";
            let fault = CarrierFault::DocumentCap;
            self.set_phase(PeerPhase::Cooldown, Some(fault.reason()));
            return self.report(fault, detail);
        }
        self.open_peer(now_ms)
    }

    /// Mint a peer attempt from the live grant and ask the host to open it.
    fn open_peer(&mut self, now_ms: u64) -> Vec<CarrierEffect> {
        let Some(grant) = self.grant.live_grant(now_ms).cloned() else {
            self.park_keeping_terminal_reason(PeerPhase::AwaitingGrant);
            return Vec::new();
        };
        if !grant.admits(TerminalTransport::Peer) {
            let fault = CarrierFault::Disabled;
            self.set_phase(PeerPhase::Disabled, Some(fault.reason()));
            return self.report(fault, "this grant cannot open a peer carrier");
        }
        self.next_attempt_id += 1;
        let attempt = PeerAttempt {
            attempt_id: self.next_attempt_id,
            worker_fp: self.worker_fp.clone(),
            worker_epoch: grant.worker_epoch.clone(),
            transport: TerminalTransport::Peer,
            // Opaque, browser-allocated, exactly as v2 mints it: it names THIS
            // negotiation, and is the third half of the tuple a `Ready` matches.
            peer_id: format!("peer-{}", self.next_attempt_id),
            grant_id: grant.grant_id.clone(),
            tab_id: grant.tab_id.clone(),
            device_fingerprint: grant.device_fingerprint.clone(),
            stun_urls: grant.stun_urls.clone(),
            session_ids: self.demand.clone(),
        };
        self.attempt = Some(attempt.clone());
        self.attempt_started_ms = now_ms;
        self.set_phase(PeerPhase::Gathering, None);
        vec![CarrierEffect::OpenTransport { attempt }]
    }

    /// The transport produced a local offer.
    fn offer(&mut self, attempt_id: u64, offer_sdp: String) -> Vec<CarrierEffect> {
        if self.attempt_id() != Some(attempt_id) {
            return Vec::new();
        }
        if !sdp_is_usable(&offer_sdp) {
            let detail = "terminal peer offer has no usable candidates";
            return self.fault(attempt_id, CarrierFault::InvalidOffer, detail);
        }
        self.set_phase(PeerPhase::Negotiating, None);
        vec![CarrierEffect::NegotiateOffer {
            attempt_id,
            offer_sdp,
        }]
    }

    /// The coordinator answered. The tuple is checked before the SDP because
    /// they are different faults.
    fn answer(&mut self, attempt_id: u64, answer: PeerAnswer) -> Vec<CarrierEffect> {
        let Some(open) = self.attempt.clone() else {
            return Vec::new();
        };
        if open.attempt_id != attempt_id {
            return Vec::new();
        }
        if let Some(fault) = answer_fault(&answer, &open) {
            let detail = match fault {
                CarrierFault::IdentityMismatch => "invalid peer negotiation response",
                _ => "terminal peer answer has no usable candidates",
            };
            return self.fault(attempt_id, fault, detail);
        }
        self.set_phase(PeerPhase::Authenticating, None);
        let answer_sdp = answer.answer_sdp;
        vec![CarrierEffect::ApplyAnswer {
            attempt_id,
            answer_sdp,
        }]
    }

    /// A refusal arrived. One naming no attempt is a COORDINATOR failure: the
    /// offer went out, no answer came back, and the attempt is what is over.
    fn refused(&mut self, attempt_id: Option<u64>, reason: &str) -> Vec<CarrierEffect> {
        let Some(live) = self.attempt_id() else {
            return Vec::new();
        };
        if attempt_id.is_some_and(|named| named != live) {
            return Vec::new();
        }
        // A named one is the worker's own reason: only the eight the protocol
        // fixes are claimed as such.
        let fault = match attempt_id {
            Some(_) => classify_worker_reason(reason).unwrap_or(CarrierFault::InvalidAnswer),
            None => CarrierFault::CoordinatorUnavailable,
        };
        self.fault(live, fault, reason)
    }

    /// The far end proved its tuple. The attempt stays OPEN afterwards: a peer
    /// that authenticated can still lose ICE, and the close must name it.
    fn authenticated(&mut self, attempt_id: u64, ready: ReadyTuple) -> Vec<CarrierEffect> {
        let Some(open) = self.attempt.clone() else {
            return Vec::new();
        };
        if open.attempt_id != attempt_id {
            return Vec::new();
        }
        if let Some(fault) = ready_fault(&ready, &open) {
            let detail = "terminal peer Ready did not match its authenticated tuple";
            return self.fault(attempt_id, fault, detail);
        }
        self.peer_held = true;
        self.set_phase(PeerPhase::Candidate, None);
        vec![CarrierEffect::StageCarrier { attempt_id, ready }]
    }

    /// A view started or stopped wanting this session here.
    fn demand(&mut self, session_id: String, active: bool) -> Vec<CarrierEffect> {
        if !active {
            self.active_views = self.active_views.saturating_sub(1);
            self.demand.remove(&session_id);
            self.grant.step(GrantInput::DemandRemoved { session_id });
            return Vec::new();
        }
        self.active_views += 1;
        self.demand.insert(session_id.clone());
        let mut out = self.grant.step(GrantInput::DemandAdded { session_id });
        out.extend(self.start(self.now_ms));
        out
    }

    fn grant_step(&mut self, grant: GrantInput) -> Vec<CarrierEffect> {
        let minted = matches!(&grant, GrantInput::Minted(_));
        let mut out = self.grant.step(grant);
        if minted {
            self.grant.refresh_at_ms = Some(self.now_ms);
        }
        out.extend(self.start(self.now_ms));
        out
    }

    /// One pass over this worker's deadlines.
    fn sweep(&mut self, now_ms: u64) -> Vec<CarrierEffect> {
        self.now_ms = now_ms;
        // The EXPIRED rule is decided from the CLIENT's clock, not a worker's
        // word: a credential this client knows is dead is not one to
        // authenticate on, and waiting to be told keeps a dead secret alive.
        let mut out = match self.grant.sweep(now_ms) {
            GrantSweep::Nothing => Vec::new(),
            GrantSweep::RenewDue => self.grant.step(GrantInput::RenewDue { now_ms }),
            GrantSweep::Expired => return self.expire_grant(),
        };
        // The negotiation deadline covers gathering and the offer's round trip
        // through the coordinator. An authenticated peer is not negotiating, and
        // retiring it for a slow attempt would drop a live session.
        let negotiating = matches!(self.phase, PeerPhase::Gathering | PeerPhase::Negotiating);
        let waited = now_ms.saturating_sub(self.attempt_started_ms);
        if negotiating
            && waited >= TERMINAL_PEER_NEGOTIATION_DEADLINE_MS
            && let Some(id) = self.attempt_id()
        {
            out.extend(self.fault(id, CarrierFault::InvalidOffer, "negotiation deadline"));
        }
        out
    }

    /// The attempt is over. Everything after is the four fault rules and the
    /// fallback, and nothing here touches the session's Sync authority.
    fn fault(&mut self, attempt_id: u64, fault: CarrierFault, detail: &str) -> Vec<CarrierEffect> {
        if self.attempt_id() != Some(attempt_id) {
            return Vec::new();
        }
        let mut out = vec![CarrierEffect::CloseAttempt {
            attempt_id,
            reason: detail.to_string(),
        }];
        self.attempt = None;
        self.peer_held = false;
        if self.phase == PeerPhase::Active {
            self.faults.hold_down_from_active(self.now_ms);
        }
        // The only branch that is per-fault: a credential a worker revoked is
        // dropped, and one never shown to be bad is kept.
        if !fault.keeps_grant() {
            let now_ms = self.now_ms;
            out.extend(self.grant.step(GrantInput::Revoked { fault, now_ms }));
        }
        self.set_phase(PeerPhase::Cooldown, Some(fault.reason()));
        out.extend(self.report(fault, detail));
        out
    }
}
