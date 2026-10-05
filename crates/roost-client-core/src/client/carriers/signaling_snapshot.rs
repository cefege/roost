//! What a host may OBSERVE about one worker's peer attempt, and the interface
//! it observes it through: the accessors the carrier registry reads, and
//! `PeerSignalling`, the lane the route table drives. Every field here is a
//! projection of state `super::signaling` owns, and no transition lives in this
//! file — a host that learned to change an attempt through it would have two
//! writers for one state machine.

use std::collections::BTreeSet;

use crate::client::carriers::faults::FallbackReason;
use crate::client::carriers::grant::GrantPhase;
use crate::client::carriers::loopback::LoopbackProbe;
use crate::client::carriers::signaling::Signalling;
use crate::client::carriers::transport_trait::PeerSignalling;
use crate::client::carriers::{CarrierEffect, CarrierEnvironment, PeerPhase, SignallingInput};
use crate::terminal::token::TerminalTransport;

/// Which kind of ICE candidate a live peer is paired on, as the route
/// diagnostic spells it.
///
/// `None` is a real value rather than an absence: a loopback route has no
/// candidate and a Sync route has no carrier at all, and a reader that cannot
/// tell those two apart from a peer whose candidate has not been read yet is
/// reading a number that means three things.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CandidateType {
    /// No peer, so no candidate.
    #[default]
    None,
    /// A host address, the LAN case.
    Host,
    /// A server-reflexive address, discovered through STUN.
    Srflx,
    /// A peer-reflexive address, which only a successful pairing produces.
    Prflx,
}

impl CandidateType {
    /// The spelling v2's route entry uses, and the one a reader compares.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Host => "host",
            Self::Srflx => "srflx",
            Self::Prflx => "prflx",
        }
    }
}

/// What the transport measured about the live peer behind a route.
///
/// Every field is an `Option` or a `None` variant because a MEASUREMENT is not a
/// constant: a peer that has not authenticated has no round trip, a browser
/// that exposes no stats has no buffer count, and a zero in either place would
/// be a number a reader could not tell from a real one.
///
/// The transport writes it and nothing else does, and it is a projection
/// rather than state the machine steps on — a measurement that could move a
/// phase would be a second input the machine's ordering would have to know
/// about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerTelemetry {
    /// The browser-allocated id of the live negotiation, when there is one.
    pub peer_id: Option<String>,
    /// Which kind of candidate the selected pair is.
    pub candidate_type: CandidateType,
    /// When the newest transport probe was answered, on the host's clock. An
    /// instant rather than an age, because an age is wrong the moment after it
    /// is recorded and a reader compares against its own clock.
    pub last_probe_at_ms: Option<u64>,
    /// Whether the peer answered its newest probe inside the qualification
    /// window. The host re-records it when the window passes, so a reader
    /// without a clock still sees a peer that stopped answering as unqualified.
    pub liveness_qualified: bool,
    /// The peer's current round trip.
    pub rtt_ms: Option<u64>,
    /// The round trip on the worker's CONTROL lane specifically, which is not
    /// the data lane's and is the number a keystroke's latency is made of.
    pub worker_control_rtt_ms: Option<u64>,
    /// What the transport is holding for this peer and cannot write yet.
    pub buffered_bytes: Option<u64>,
}

impl Signalling {
    /// The gate that decides whether a peer is needed, and this machine's one
    /// view of a staged loopback carrier.
    pub fn loopback_probe(&self) -> &LoopbackProbe {
        &self.loopback
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

    /// The sessions a view currently wants here.
    pub fn demanded_sessions(&self) -> &BTreeSet<String> {
        &self.demand
    }

    /// Where the attempt is.
    pub fn phase(&self) -> PeerPhase {
        self.phase
    }

    /// Replace the environment every gate in `start` reads.
    ///
    /// A setter rather than a constructor argument because both halves of it
    /// move while the machine lives: the document's WebRTC availability is
    /// fixed, but the peer count is not, and a machine that read the count once
    /// would enforce a cap frozen at the moment it was built.
    pub fn set_environment(&mut self, env: CarrierEnvironment) {
        self.env = env;
    }

    /// Record what the transport measured. Never moves a phase — see
    /// `PeerTelemetry`.
    pub fn set_telemetry(&mut self, telemetry: PeerTelemetry) {
        self.telemetry = telemetry;
    }
}
impl PeerSignalling for Signalling {
    fn worker_fp(&self) -> &str {
        &self.worker_fp
    }

    fn snapshot(&self) -> SignallingSnapshot {
        SignallingSnapshot {
            worker_fp: self.worker_fp.clone(),
            phase: self.phase,
            fallback_reason: self.faults.reason,
            active_views: self.active_views,
            demanded_sessions: self.demand.clone(),
            prewarmed: !self.prewarm_sessions.is_empty(),
            has_carrier: self.peer_held,
            transport_held: self.peer_held.then_some(TerminalTransport::Peer),
            peers_allocated: self.env.peers_allocated,
            grant_phase: self.grant.phase(),
            sync_generation: self.env.sync_generation,
            // The grant's own refusal is a reason the carrier never opened, and
            // the most common one there is; a reader shown only the fault's
            // detail sees `None` on a session that is waiting out a
            // thirty-second retry, and calls it healthy.
            last_failure_detail: self
                .faults
                .last_detail
                .clone()
                .or_else(|| self.grant.last_detail().map(str::to_string)),
            telemetry: self.telemetry.clone(),
            time_to_direct_ms: self.time_to_direct_ms,
            direct_phase_ms: self.direct_phase_timings(),
        }
    }

    fn phase(&self) -> PeerPhase {
        self.phase
    }

    fn step(&mut self, input: SignallingInput) -> Vec<CarrierEffect> {
        Signalling::step(self, input)
    }
}

/// What a host can see about one worker's direct-carrier attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignallingSnapshot {
    /// Which worker.
    pub worker_fp: String,
    /// Where the attempt is.
    pub phase: PeerPhase,
    /// The coarse recorded reason, cleared on every successful transition.
    pub fallback_reason: Option<FallbackReason>,
    /// How many live views want a session on this worker.
    pub active_views: u64,
    /// Which sessions they want.
    pub demanded_sessions: BTreeSet<String>,
    /// Whether pre-warm wants this worker's peer held ready, with or without a
    /// view also asking for it.
    pub prewarmed: bool,
    /// Whether an authenticated carrier is held for this worker.
    pub has_carrier: bool,
    /// Which kind of carrier it is.
    pub transport_held: Option<TerminalTransport>,
    /// How many WebRTC peers this document holds, across every worker.
    pub peers_allocated: u32,
    /// Where the grant is.
    pub grant_phase: GrantPhase,
    /// The Sync generation this worker's sessions are fenced to. Reported and
    /// never written: nothing here can move it, and nothing emits its teardown.
    pub sync_generation: u64,
    /// The host's own last failure detail, never a value that failed to match.
    pub last_failure_detail: Option<String>,
    /// What the transport measured about the live peer. A default on a machine
    /// with no peer, which is a value and not a placeholder.
    pub telemetry: PeerTelemetry,
    /// How long this worker's views waited before the peer now serving them
    /// was elected. The MACHINE's measurement, not the transport's, which is
    /// why it is not a `PeerTelemetry` field: the host replaces that record
    /// whole on every heartbeat.
    pub time_to_direct_ms: Option<u64>,
    /// How far into the open attempt each phase toward direct was entered.
    pub direct_phase_ms: DirectPhaseTimings,
}

/// Milliseconds from an attempt's start to its first entry into each phase of
/// the climb to a direct route; `None` for a phase it has not reached.
///
/// Every transport observation advances the machine's clock first, so each
/// stamp is the instant the host reported the transition, not the last sweep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirectPhaseTimings {
    /// ICE gathering began.
    pub gathering_ms: Option<u64>,
    /// The offer went to the coordinator.
    pub negotiating_ms: Option<u64>,
    /// The answer was applied.
    pub authenticating_ms: Option<u64>,
    /// The carrier authenticated.
    pub candidate_ms: Option<u64>,
    /// The carrier was elected.
    pub active_ms: Option<u64>,
}

impl SignallingSnapshot {
    /// A snapshot of a worker that has attempted nothing.
    pub fn idle(worker_fp: String) -> Self {
        Self {
            worker_fp,
            phase: PeerPhase::Idle,
            fallback_reason: None,
            active_views: 0,
            demanded_sessions: BTreeSet::new(),
            prewarmed: false,
            has_carrier: false,
            transport_held: None,
            peers_allocated: 0,
            grant_phase: GrantPhase::Absent,
            sync_generation: 0,
            last_failure_detail: None,
            telemetry: PeerTelemetry::default(),
            time_to_direct_ms: None,
            direct_phase_ms: DirectPhaseTimings::default(),
        }
    }
}
