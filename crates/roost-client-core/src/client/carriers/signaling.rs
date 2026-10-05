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
    FaultState, answer_fault, classify_coordinator_refusal, classify_worker_reason, ready_fault,
};
use crate::client::carriers::grant::{GrantInput, GrantLifecycle, GrantPhase, GrantSweep};
use crate::client::carriers::loopback::LoopbackProbe;
use crate::client::carriers::signaling_demand::DirectWait;
use crate::client::carriers::signaling_snapshot::PeerTelemetry;
use crate::client::carriers::{
    CarrierEffect, CarrierEnvironment, CarrierFault, PeerAnswer, PeerAttempt, PeerPhase,
    ReadyTuple, SignallingInput,
};

/// One worker's WebRTC attempt, and the election around it.
#[derive(Debug)]
pub struct Signalling {
    pub(crate) worker_fp: String,
    pub(crate) demand: BTreeSet<String>,
    pub(crate) active_views: u64,
    /// The sessions pre-warm holds a grant and a peer ready for, with no view
    /// asking yet. Empty while the worker is not pre-warmed; a non-empty set
    /// opens `start`'s gate exactly as one view would.
    pub(crate) prewarm_sessions: BTreeSet<String>,
    pub(crate) env: CarrierEnvironment,
    pub(crate) grant: GrantLifecycle,
    pub(crate) loopback: LoopbackProbe,
    pub(crate) faults: FaultState,
    pub(crate) phase: PeerPhase,
    pub(crate) attempt: Option<PeerAttempt>,
    /// The offer SDP the transport read while no grant was live, sent the
    /// moment `adopt_grant` copies a mint into the attempt.
    pub(crate) held_offer: Option<String>,
    /// What the transport last measured. Cleared when the attempt it belongs
    /// to ends, so a reader never sees a dead peer's round trip on a route
    /// that has fallen back to Sync.
    pub(crate) telemetry: PeerTelemetry,
    /// When this worker's views last began waiting for a direct route; `None`
    /// while none waits. The wait and its measurement are
    /// `super::signaling_demand`'s.
    pub(crate) direct_wait: Option<DirectWait>,
    /// How long the views waited before the peer now serving them was
    /// elected. Cleared with the attempt, for `telemetry`'s reason.
    pub(crate) time_to_direct_ms: Option<u64>,
    pub(crate) attempt_started_ms: u64,
    /// When the open attempt first entered each phase, by `PeerPhase::index`.
    pub(crate) phase_entered_ms: [Option<u64>; PeerPhase::COUNT],
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
            prewarm_sessions: BTreeSet::new(),
            env,
            faults: FaultState::default(),
            phase: PeerPhase::Idle,
            attempt: None,
            held_offer: None,
            attempt_started_ms: 0,
            phase_entered_ms: [None; PeerPhase::COUNT],
            peer_held: false,
            now_ms,
            next_attempt_id: 0,
            telemetry: PeerTelemetry::default(),
            direct_wait: None,
            time_to_direct_ms: None,
            retired: false,
        }
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
                session_id,
                active,
                now_ms,
                ..
            } => out.extend(self.demand(session_id, active, now_ms)),
            SignallingInput::Prewarm {
                session_ids,
                now_ms,
            } => out.extend(self.prewarm(session_ids, now_ms)),
            SignallingInput::PrewarmReleased { now_ms } => out.extend(self.release_prewarm(now_ms)),
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
                peer_id,
                offer_sdp,
            } => out.extend(self.offer(attempt_id, peer_id, offer_sdp)),
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
            SignallingInput::PromotionCommitted { token, now_ms, .. } => {
                self.promotion_committed(&token, now_ms)
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
    pub(crate) fn start(&mut self, now_ms: u64) -> Vec<CarrierEffect> {
        let wanted = self.active_views > 0 || !self.prewarm_sessions.is_empty();
        if !wanted || self.peer_held || self.retired || self.attempt.is_some() {
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
        // A transport may gather while the first mint is in flight, using the
        // STUN servers the coordinator advertised; its offer is held until the
        // grant lands. A grant retry (`Unavailable`, `Expired`) keeps waiting,
        // so a refusal does not churn peers.
        let grantless = self.grant.live_grant(now_ms).is_none();
        if grantless
            && (self.env.stun_urls.is_none() || self.grant.phase() != GrantPhase::Requested)
        {
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
            None => {
                let held = self.grant.live_grant(self.now_ms);
                classify_coordinator_refusal(reason, self.attempt.as_ref(), held)
            }
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

    fn grant_step(&mut self, grant: GrantInput) -> Vec<CarrierEffect> {
        let mut out = match &grant {
            GrantInput::Minted(minted) => self.retire_outgrown_attempt(minted),
            GrantInput::Refused { .. } if self.holds_grantless_attempt() => {
                self.close_open_attempt("grant refused")
            }
            _ => Vec::new(),
        };
        let minted = matches!(&grant, GrantInput::Minted(_));
        out.extend(self.grant.step(grant));
        if minted {
            self.grant.refresh_at_ms = Some(self.now_ms);
            out.extend(self.adopt_grant(self.now_ms));
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
        let negotiating = self.attempt.is_some()
            && matches!(
                self.phase,
                PeerPhase::AwaitingGrant | PeerPhase::Gathering | PeerPhase::Negotiating
            );
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
    pub(crate) fn fault(
        &mut self,
        attempt_id: u64,
        fault: CarrierFault,
        detail: &str,
    ) -> Vec<CarrierEffect> {
        if self.attempt_id() != Some(attempt_id) {
            return Vec::new();
        }
        let mut out = vec![CarrierEffect::CloseAttempt {
            attempt_id,
            reason: detail.to_string(),
        }];
        self.fell_back(fault);
        self.attempt = None;
        self.held_offer = None;
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
