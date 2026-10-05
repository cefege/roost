//! The direct carrier's fault taxonomy and the machine's teardown half: what
//! went wrong, who serves the session instead, and how an attempt ends. Owned
//! by `client::carriers`. The four faults the direct-terminal oracle names
//! (`smoke/terminal/terminal-peer.spec.ts:263`) are four SEPARATE rules,
//! differing in the only way that matters afterwards: whether the grant the
//! attempt was using is still a credential.
//!
//! What a fault LEAVES BEHIND — the handover, the hold-down, the backoff and
//! the machine state a fault moves — is `faults::state`.

mod state;

pub use state::{FaultState, PEER_HOLD_DOWN_MS, fallback_effects, retry_delay_ms};

use roost_protocol::terminal_peer::sdp::inspect_terminal_peer_sdp;

use crate::client::carriers::grant::GrantInput;
use crate::client::carriers::signaling::Signalling;
use crate::client::carriers::signaling_snapshot::PeerTelemetry;
use crate::client::carriers::{
    CarrierEffect, DirectGrant, PeerAnswer, PeerAttempt, PeerPhase, ReadyTuple,
};
use crate::terminal::token::{TerminalToken, TerminalTransport};

/// Why a direct attempt is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CarrierFault {
    /// The document cannot do WebRTC: no secure context, or no peer
    /// constructor. Retrying changes neither, so this is terminal.
    Unsupported,
    /// The worker answered that it does not offer the peer carrier at all.
    Disabled,
    /// This document holds its maximum peers, or the worker has no slot left.
    DocumentCap,
    /// The worker refused the offer as unreadable (`invalid_offer`), or the
    /// browser's own offer carried no usable candidate. The grant is untouched:
    /// the offer never became a peer.
    InvalidOffer,
    /// The worker has no live grant for this device and tab
    /// (`grant_unavailable`). The credential is gone; only a fresh mint replaces.
    GrantUnavailable,
    /// The grant existed and its deadline passed (`expired`). Also gone, and the
    /// worker process epoch it named can no longer be trusted.
    GrantExpired,
    /// A message named a different worker, worker epoch, peer, or scope than the
    /// attempt presented — or the peer closed before it proved any tuple at all,
    /// which the browser cannot tell from a spoofed `Ready` and must refuse the
    /// same way. The grant SURVIVES: a refused tuple is a fact about the
    /// exchange, not the credential it carried.
    IdentityMismatch,
    /// The worker process changed, or the coordinator's connection to it rolled,
    /// so the attempt's epoch is stale and the grant must be re-minted.
    WorkerSuperseded,
    /// The coordinator could not be reached for the answer.
    CoordinatorUnavailable,
    /// The answer arrived, and the browser refused its SDP before any ICE
    /// started. Distinct from `InvalidOffer`: the OFFER was readable and the far
    /// end is the one that failed to answer readably.
    InvalidAnswer,
    /// ICE failed, or two content-free probes went unanswered.
    IceFailed,
}

/// Who serves a session whose direct attempt has faulted. Never an unproven
/// loopback carrier — one exists before it is asked to carry anything, while a
/// peer has to be negotiated first, and `Sync` is never torn down by a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultFallback {
    /// A loopback carrier is already staged here, so it takes over.
    Loopback,
    /// Nothing else exists, so the session stays on Sync.
    Sync,
}

/// The coarse reason a host records. Deliberately coarser than `CarrierFault`:
/// v2 records `network_failed` for every failure before the peer is active and
/// `ice_failed` after it (`terminal-peer.ts:114,227`), so the discriminating
/// value is the `CarrierFault` recorded beside it in the same line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FallbackReason {
    /// The document cannot do WebRTC.
    Unsupported,
    /// The worker does not offer the peer carrier.
    Disabled,
    /// The document or the worker is at capacity.
    Cap,
    /// The negotiation failed: the offer, the answer, the grant, the tuple, or
    /// the coordinator carrying them.
    NetworkFailed,
    /// ICE failed after the exchange completed.
    IceFailed,
}

impl FallbackReason {
    /// The stable string a host writes into its log, and the string the smoke
    /// backdoor reads back.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Disabled => "disabled",
            Self::Cap => "cap",
            Self::NetworkFailed => "network_failed",
            Self::IceFailed => "ice_failed",
        }
    }
}

impl CarrierFault {
    /// Whether the grant this attempt used is still usable — the whole distinction
    /// between the four oracle faults. A caller that gets this wrong either
    /// re-uses a revoked credential and retries into the same refusal, or
    /// discards a good one and waits on a mint it did not need.
    pub const fn keeps_grant(self) -> bool {
        !matches!(self, Self::GrantUnavailable | Self::GrantExpired)
    }

    /// Whether an attempt may be started again. `Unsupported` and `Disabled` are
    /// properties of the document and the worker, so no retry can change them.
    pub const fn retryable(self) -> bool {
        !matches!(self, Self::Unsupported | Self::Disabled)
    }

    /// The coarse reason a host records.
    pub const fn reason(self) -> FallbackReason {
        match self {
            Self::Unsupported => FallbackReason::Unsupported,
            Self::Disabled => FallbackReason::Disabled,
            Self::DocumentCap => FallbackReason::Cap,
            Self::IceFailed => FallbackReason::IceFailed,
            Self::CoordinatorUnavailable
            | Self::InvalidAnswer
            | Self::InvalidOffer
            | Self::GrantUnavailable
            | Self::GrantExpired
            | Self::IdentityMismatch
            | Self::WorkerSuperseded => FallbackReason::NetworkFailed,
        }
    }

    /// Who serves the session now: the OTHER transport, resolved against what
    /// actually exists. A fault on a loopback carrier cannot fall back to a peer,
    /// because a peer is only allocated after the probe has said the page is on
    /// another machine — so there is no peer to hand it to.
    pub const fn fallback(self, staged_loopback: bool) -> FaultFallback {
        if staged_loopback {
            FaultFallback::Loopback
        } else {
            FaultFallback::Sync
        }
    }

    /// The stable string for an incident log, and the worker's own reason code
    /// where the protocol fixes one.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Disabled => "disabled",
            Self::DocumentCap => "cap",
            Self::InvalidOffer => "invalid_offer",
            Self::GrantUnavailable => "grant_unavailable",
            Self::GrantExpired => "expired",
            Self::IdentityMismatch => "identity_mismatch",
            Self::WorkerSuperseded => "connection_superseded",
            Self::CoordinatorUnavailable => "coordinator_unavailable",
            Self::InvalidAnswer => "invalid_answer",
            Self::IceFailed => "ice_failed",
        }
    }
}

/// The fault one of the protocol's eight worker reason codes names. `None` for
/// anything else: an unrecognised reason is NOT mapped to a nearby member, the
/// caller reports it as a coordinator or transport failure, and claiming a
/// worker said `grant_unavailable` when it said something else is how a
/// credential gets dropped for nothing.
pub fn classify_worker_reason(reason: &str) -> Option<CarrierFault> {
    match reason {
        "disabled" => Some(CarrierFault::Disabled),
        "native_unavailable" | "invalid_offer" => Some(CarrierFault::InvalidOffer),
        "grant_unavailable" => Some(CarrierFault::GrantUnavailable),
        "expired" => Some(CarrierFault::GrantExpired),
        "capacity" => Some(CarrierFault::DocumentCap),
        "connection_superseded" => Some(CarrierFault::WorkerSuperseded),
        "ice_failed" => Some(CarrierFault::IceFailed),
        _ => None,
    }
}

/// The coordinator's denial of an offer spent on a grant it no longer holds —
/// a restarted worker's new epoch revokes every grant minted for the old one.
const COORDINATOR_GRANT_UNAVAILABLE: &str = "terminal peer grant is unavailable";

/// The fault a coordinator's refusal of an offer names. Only its grant denial,
/// for the credential this worker STILL holds, drops that credential so the
/// next attempt mints one the coordinator honours (v2 `terminal-peer.ts`
/// `negotiate`'s catch, `state.grant === grant`); a denial of a grant already
/// replaced says nothing about its successor, and every other refusal is the
/// coordinator's own failure.
pub fn classify_coordinator_refusal(
    reason: &str,
    attempt: Option<&PeerAttempt>,
    held: Option<&DirectGrant>,
) -> CarrierFault {
    let spent_held = attempt.zip(held).is_some_and(|(open, held)| {
        open.grant_id == held.grant_id
            && open.worker_epoch == held.worker_epoch
            && open.session_ids == held.session_ids
    });
    if spent_held && reason.contains(COORDINATOR_GRANT_UNAVAILABLE) {
        CarrierFault::GrantUnavailable
    } else {
        CarrierFault::CoordinatorUnavailable
    }
}

/// Whether an offer or answer is readable AND carries somewhere to connect. v2
/// checks this before it puts an offer on the wire or applies an answer
/// (`terminal-peer-connection.ts:142,147`). The inspection is `roost_protocol`'s;
/// this is the "and there is a candidate" half — a different rule from "and it
/// parses", and a different fault.
pub fn sdp_is_usable(sdp: &str) -> bool {
    inspect_terminal_peer_sdp(sdp).is_ok_and(|metadata| metadata.candidate_count > 0)
}

/// The fault a coordinator's answer names, or `None` when it is this attempt's.
/// Its own readability is NOT decided here: an answer bound to another peer is a
/// negotiation that was never ours, and an unreadable one is one that was.
pub fn answer_fault(answer: &PeerAnswer, attempt: &PeerAttempt) -> Option<CarrierFault> {
    if !answer.binds(attempt) {
        return Some(CarrierFault::IdentityMismatch);
    }
    if !sdp_is_usable(&answer.answer_sdp) {
        return Some(CarrierFault::InvalidAnswer);
    }
    None
}

/// The fault a `Ready` names, or `None` when it is this attempt's.
pub fn ready_fault(ready: &ReadyTuple, attempt: &PeerAttempt) -> Option<CarrierFault> {
    if ready.admits(attempt) {
        return None;
    }
    Some(CarrierFault::IdentityMismatch)
}

/// The teardown half of the machine: how an attempt ends and what is left. An
/// `impl Signalling` block here rather than in `signaling` because these are the
/// fence between "a message arrived" and "a session loses its direct route",
/// which is what the four fault rules are about. The machine owns the
/// negotiation, this owns the ending.
impl Signalling {
    /// Close whatever the worker holds and never ask for it again. Not a fault:
    /// a removed worker will not come back under this fingerprint, so a retry
    /// here is a request the coordinator can only refuse.
    pub(crate) fn retire(&mut self) -> Vec<CarrierEffect> {
        let mut out = self.close_open_attempt("worker retired");
        out.extend(self.grant.step(GrantInput::WorkerRetired));
        self.peer_held = false;
        self.set_phase(PeerPhase::Disabled, None);
        self.retired = true;
        out
    }

    /// Close the open attempt, if there is one, naming the reason it went.
    pub(crate) fn close_open_attempt(&mut self, reason: &str) -> Vec<CarrierEffect> {
        // The measurements belong to the attempt, so they go when the attempt
        // does: a dead peer's round trip left on a route that has fallen back
        // to Sync reads as a live one.
        self.telemetry = PeerTelemetry::default();
        self.time_to_direct_ms = None;
        self.phase_entered_ms = [None; PeerPhase::COUNT];
        self.held_offer = None;
        self.attempt.take().map_or_else(Vec::new, |attempt| {
            vec![CarrierEffect::CloseAttempt {
                attempt_id: attempt.attempt_id,
                reason: reason.to_string(),
            }]
        })
    }

    /// A fresh mint arrived while an attempt that has not proved its tuple is
    /// open on the previous one. The worker now holds the NEW scope, so the
    /// `Ready` this attempt will get names sessions outside the one it
    /// presented: v2's `updateGrant` refuses exactly that, and `presentGrant`
    /// closes the connection for `maybeStart` to renegotiate on the new grant.
    /// An authenticated peer is widened by the route registry instead.
    pub(crate) fn retire_outgrown_attempt(&mut self, minted: &DirectGrant) -> Vec<CarrierEffect> {
        if minted.worker_fp != self.worker_fp || self.peer_held {
            return Vec::new();
        }
        let Some(open) = self.attempt.as_ref() else {
            return Vec::new();
        };
        // A grantless attempt was opened FOR this mint and adopts it; faulting
        // it as outgrown is the "terminal peer grant changed" loop.
        if open.grant_id.is_empty() {
            return Vec::new();
        }
        let still_admitted = minted.admits(TerminalTransport::Peer)
            && minted.worker_epoch == open.worker_epoch
            && minted.session_ids.is_subset(&open.session_ids);
        if still_admitted {
            return Vec::new();
        }
        let attempt_id = open.attempt_id;
        self.fault(
            attempt_id,
            CarrierFault::IdentityMismatch,
            "terminal peer grant changed",
        )
    }

    /// Record a phase and its reason together, because a phase paired with one
    /// reason and another paired with a different one is two states a host
    /// cannot read off a snapshot naming both.
    pub(crate) fn set_phase(&mut self, phase: PeerPhase, reason: Option<FallbackReason>) {
        if self.phase == phase && self.faults.reason == reason {
            return;
        }
        if self.phase != phase {
            self.phase_entered_ms[phase.index()] = Some(self.now_ms);
        }
        self.phase = phase;
        self.faults.reason = reason;
    }

    /// Park on a gate that is only HOLDING the attempt, keeping a reason no
    /// retry will change.
    ///
    /// The loopback probe and the grant lifecycle are both gates an attempt
    /// waits at, and `start` runs on every tick. Parking with no reason is
    /// right the first time and wrong afterwards: a document that cannot do
    /// WebRTC and a worker that does not offer the carrier are facts about
    /// this machine, and clearing them on the next tick turns the one answer
    /// an operator can act on back into a blank field. The attempt re-reports
    /// the reason the moment it is allowed to try again, so nothing is claimed
    /// that has not been established.
    pub(crate) fn park_keeping_terminal_reason(&mut self, phase: PeerPhase) {
        let kept = match self.faults.reason {
            Some(reason @ (FallbackReason::Unsupported | FallbackReason::Disabled)) => Some(reason),
            _ => None,
        };
        self.set_phase(phase, kept);
    }

    /// The open attempt's id. Every event is matched against it, so a message
    /// for a dead attempt is dropped rather than applied to whatever came after.
    pub(crate) fn attempt_id(&self) -> Option<u64> {
        self.attempt.as_ref().map(|attempt| attempt.attempt_id)
    }

    /// Whether `token` names a route on the peer this machine holds. A promotion
    /// on another connection is not this machine's news, and treating one as an
    /// activation is how a machine that never held the session reports Active.
    pub(crate) fn holds(&self, token: &TerminalToken) -> bool {
        self.peer_held
            && token.worker_fp.as_deref() == Some(self.worker_fp.as_str())
            && token.transport == TerminalTransport::Peer
    }

    /// The grant died on its own clock, with no worker to say so.
    pub(crate) fn expire_grant(&mut self) -> Vec<CarrierEffect> {
        let detail = "direct grant reached its deadline";
        let fault = CarrierFault::GrantExpired;
        if self.attempt.is_some() {
            self.fell_back(fault);
        }
        let mut out = self.close_open_attempt(detail);
        let now_ms = self.now_ms;
        out.extend(self.grant.step(GrantInput::Revoked { fault, now_ms }));
        self.peer_held = false;
        self.set_phase(PeerPhase::Cooldown, Some(fault.reason()));
        out.extend(self.report(fault, detail));
        out
    }

    /// Hand every session to the other transport, log which rule fired, arm the
    /// retry. Nothing here re-opens, re-dials or re-subscribes anything.
    pub(crate) fn report(&mut self, fault: CarrierFault, detail: &str) -> Vec<CarrierEffect> {
        let staged = self.loopback.has_staged_carrier();
        let demand = &self.demand;
        self.faults
            .report(fault, &self.worker_fp, detail, self.now_ms, staged, demand)
    }
}
