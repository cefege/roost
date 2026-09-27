//! The direct carrier's fault taxonomy and the machine's teardown half: what
//! went wrong, what each fault leaves behind, who serves the session instead,
//! and how an attempt ends. Owned by `client::carriers`. The four faults the
//! direct-terminal oracle names (`smoke/terminal/terminal-peer.spec.ts:263`) are
//! four SEPARATE rules, differing in the only way that matters afterwards:
//! whether the grant the attempt was using is still a credential.

use std::collections::BTreeSet;

use roost_protocol::terminal_peer::sdp::inspect_terminal_peer_sdp;

use crate::client::carriers::grant::GrantInput;
use crate::client::carriers::signaling::Signalling;
use crate::client::carriers::{
    CarrierEffect, PeerAnswer, PeerAttempt, PeerPhase, ReadyTuple,
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
    Sync,}

/// The coarse reason a host records. Deliberately coarser than `CarrierFault`:
/// v2 emits exactly one reason, `network_failed`, for every negotiation failure
/// (`terminal-peer.ts:114`), so the discriminating value is the `CarrierFault`
/// recorded beside it in the same line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FallbackReason {
    /// The document cannot do WebRTC.
    Unsupported,
    /// The worker does not offer the peer carrier.
    Disabled,
    /// The document or the worker is at capacity.
    Cap,
    /// The negotiation failed: the offer, the answer, the grant, or the tuple.
    NetworkFailed,
    /// The coordinator was not reachable.
    CoordinatorUnavailable,
    /// The far end's answer did not parse.
    InvalidResponse,
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
            Self::CoordinatorUnavailable => "coordinator_unavailable",
            Self::InvalidResponse => "invalid_response",
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
            Self::CoordinatorUnavailable => FallbackReason::CoordinatorUnavailable,
            Self::InvalidAnswer => FallbackReason::InvalidResponse,
            Self::IceFailed => FallbackReason::IceFailed,
            Self::InvalidOffer
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

/// What a fault owes its sessions: the other transport, and nothing else.
/// Nothing here re-opens, re-dials, or re-subscribes anything, and that absence
/// is the point rather than an omission. A fault costs a session its direct route
/// and nothing else (`protocol/spec/direct-terminal.md:29`): the PTY lives in
/// the worker and no client can recreate it, but a client that dropped the
/// session's Sync authority, re-dialled the socket, or re-subscribed a domain
/// would make the worker look like it had lost the session — so "the PTY is not
/// recreated" is observable HERE, as the absence of exactly those effects.
pub fn fallback_effects(
    fault: CarrierFault,
    staged: bool,
    sessions: &BTreeSet<String>,
) -> Vec<CarrierEffect> {
    let transport = fault.fallback(staged);
    let fallback = |session_id: &String| CarrierEffect::Fallback {
        session_id: session_id.clone(),
        transport,
    };
    sessions.iter().map(fallback).collect()
}

// The first retry delay, and the ceiling it grows to. The ceiling is the part
// that is a contract: a worker that keeps failing is still retried, and at a rate
// a fleet-sized browser tab can afford.
const RETRY_BASE_MS: u64 = 1_000;
const RETRY_MAX_MS: u64 = 30_000;

/// How long a peer's failure holds its worker before a new peer is allocated. v2
/// holds a worker down for 30 s once its peer has been ACTIVE
/// (`terminal-peer.ts:24,275`): a peer that dropped after serving sessions is far
/// likelier to drop again than one that never came up.
pub const PEER_HOLD_DOWN_MS: u64 = 30_000;

/// Bounded exponential backoff for the peer attempt loop: 1s, 2s, 4s, capped at
/// 30s — the shape v2 runs through `backoffDelayMs(failureCount - 1,
/// { baseMs: 1_000, maxMs: 30_000 })` (`terminal-peer.ts:277`), spelled out
/// because that helper has no v3 owner.
pub fn retry_delay_ms(attempt: u32) -> u64 {
    RETRY_BASE_MS
        .saturating_mul(1u64 << attempt.min(16))
        .min(RETRY_MAX_MS)
}

/// The parts of a machine's state that only a FAULT moves. Split out because
/// they are the same for every fault, and inline they would repeat the hold-down
/// and the backoff per call site — which is how two drift apart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FaultState {
    pub failure_count: u32,
    /// Until when a worker whose peer was ELECTED is held down.
    pub hold_down_until_ms: u64,
    pub retry_at_ms: u64,
    /// The coarse recorded reason, cleared on every successful transition.
    pub reason: Option<FallbackReason>,
    /// The host's own last failure detail, never a value that failed to match.
    pub last_detail: Option<String>,
}

impl FaultState {
    /// A promotion committed, so the failure run is over.
    pub fn cleared(&mut self) {
        self.failure_count = 0;
        self.last_detail = None;
    }

    /// The worker had an ELECTED peer, so hold it down before allocating another.
    pub fn hold_down_from_active(&mut self, now_ms: u64) {
        self.hold_down_until_ms = now_ms.saturating_add(PEER_HOLD_DOWN_MS);
    }

    /// Arm the retry. The core owns no timer, so this reports the INSTANT, not a
    /// delay: a host that cannot schedule one leaves the session on Sync.
    pub fn schedule(&mut self, at_ms: u64) -> Vec<CarrierEffect> {
        self.retry_at_ms = at_ms;
        vec![CarrierEffect::RetryAt { at_ms }]
    }

    /// Report a fault: every session it carried falls back to the other
    /// transport, the host logs which rule fired, and the retry is armed. The
    /// attempt's closure and the grant's revocation happen BEFORE this and are
    /// the caller's, because both need the machine's own state — which is why
    /// this half, the same for every fault, is the one a new member cannot skip.
    pub fn report(
        &mut self,
        fault: CarrierFault,
        worker_fp: &str,
        detail: &str,
        now_ms: u64,
        staged: bool,
        sessions: &BTreeSet<String>,
    ) -> Vec<CarrierEffect> {
        self.failure_count = self.failure_count.saturating_add(1);
        self.last_detail = Some(detail.to_string());
        let mut out = fallback_effects(fault, staged, sessions);
        out.push(CarrierEffect::Faulted {
            worker_fp: worker_fp.to_string(),
            fault,
            detail: detail.to_string(),
        });
        if !fault.retryable() {
            return out;
        }
        let backoff = now_ms.saturating_add(retry_delay_ms(self.failure_count - 1));
        let at_ms = self.hold_down_until_ms.max(backoff);
        out.extend(self.schedule(at_ms));
        out
    }
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
    fn close_open_attempt(&mut self, reason: &str) -> Vec<CarrierEffect> {
        self.attempt.take().map_or_else(Vec::new, |attempt| {
            vec![CarrierEffect::CloseAttempt {
                attempt_id: attempt.attempt_id,
                reason: reason.to_string(),
            }]
        })
    }

    /// Record a phase and its reason together, because a phase paired with one
    /// reason and another paired with a different one is two states a host
    /// cannot read off a snapshot naming both.
    pub(crate) fn set_phase(&mut self, phase: PeerPhase, reason: Option<FallbackReason>) {
        if self.phase == phase && self.faults.reason == reason {
            return;
        }
        self.phase = phase;
        self.faults.reason = reason;
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
        self.faults.report(fault, &self.worker_fp, detail, self.now_ms, staged, demand)
    }
}
