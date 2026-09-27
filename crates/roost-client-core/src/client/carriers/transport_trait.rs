//! The two seams the direct carrier is swapped behind: the state machine, and
//! the WebRTC stack underneath it. Owned by `client::carriers`, implemented by
//! `Signalling` on one side and by the browser adapter on the other. This is the
//! file that makes "the WebRTC stack misbehaves" a plan rather than a rewrite:
//! a new `PeerTransport` is one file, and driving the machine without a browser
//! is `ScriptedPeerSignalling`.

use std::fmt;

use crate::client::carriers::{
    CarrierEffect, PeerAttempt, PeerPhase, SignallingInput, SignallingSnapshot,
};
use roost_protocol::versioning::{
    CHANNEL_TERMINAL_CONTROL_V1, CHANNEL_TERMINAL_DATA_V1, CHANNEL_TERMINAL_HISTORY_V1,
};

/// One of the peer's three ordered data channels.
///
/// Ordered and negotiated in band, so the control lane's ordering is the
/// framing guarantee the packet assembler relies on; the history lane exists
/// because a multi-megabyte baseline must not sit in front of a keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeerLane {
    /// Views, input, probes, readiness, and closure.
    Control,
    /// Cell frames.
    Data,
    /// Scrollback pages.
    History,
}

impl PeerLane {
    /// Every lane, in the order the protocol numbers them. The order IS the
    /// stream id: two peers that number their channels differently will not
    /// pair their streams.
    pub const ALL: [PeerLane; 3] = [Self::Control, Self::Data, Self::History];

    /// The channel label both ends open.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Control => CHANNEL_TERMINAL_CONTROL_V1,
            Self::Data => CHANNEL_TERMINAL_DATA_V1,
            Self::History => CHANNEL_TERMINAL_HISTORY_V1,
        }
    }

    /// The stream id for this lane, matching `PeerLane::ALL`.
    pub const fn stream_id(self) -> u16 {
        match self {
            Self::Control => 0,
            Self::Data => 1,
            Self::History => 2,
        }
    }
}

/// Why a transport could not do what it was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The document cannot reach a transport of this kind at all.
    Unavailable {
        /// The host's own detail.
        detail: String,
    },
    /// The transport rejected the bytes. Nothing was written, which the caller
    /// reports as a fact rather than as an exception in the middle of an input
    /// handler.
    Refused {
        /// What the transport said.
        detail: String,
    },
    /// The transport died, or the attempt was already closed.
    Closed {
        /// Why.
        reason: String,
    },
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable { detail } => write!(formatter, "unavailable: {detail}"),
            Self::Refused { detail } => write!(formatter, "refused: {detail}"),
            Self::Closed { reason } => write!(formatter, "closed: {reason}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// The WebRTC stack, as the state machine needs it.
///
/// Everything the machine cannot decide for itself and cannot ask a host to do
/// synchronously: opening a peer, handing it an answer, writing a packet,
/// probing it, and closing it. The machine owns WHEN each of these happens; the
/// transport owns what the browser's WebRTC stack does about it.
pub trait PeerTransport {
    /// Open this attempt's transport and its ordered lanes.
    ///
    /// The offer is NOT returned here, because a browser has none yet: it fills
    /// the local description in as candidates gather, and the gathering deadline
    /// that decides when to read it belongs to the core, which owns no timer.
    /// The host calls `local_offer` once that deadline has passed, which is why
    /// `attempt_id` matters: a late read for a dead attempt is discardable.
    fn open(&mut self, attempt: &PeerAttempt) -> Result<(), TransportError>;

    /// The local description, ready to hand the coordinator, or `Unavailable`
    /// while ICE is still gathering. Candidate-filtered by the transport,
    /// because a host or mDNS candidate is an address the coordinator would
    /// otherwise relay to a worker on another machine.
    fn local_offer(&self, attempt_id: u64) -> Result<String, TransportError>;

    /// Hand the coordinator's answer to an open transport.
    fn accept_answer(&mut self, attempt_id: u64, answer_sdp: &str) -> Result<(), TransportError>;

    /// Write one already-framed packet on one lane. The framing is the CALLER's:
    /// a transport moves bytes and knows nothing about the carrier's encoding,
    /// which is what keeps the packet header out of every WebRTC stack.
    fn send(
        &mut self,
        attempt_id: u64,
        lane: PeerLane,
        bytes: &[u8],
    ) -> Result<(), TransportError>;

    /// Write one already-framed content-free probe on the control lane. Two
    /// unanswered probes retire the peer, and the count is the caller's because
    /// the reply it correlates is the caller's too.
    fn probe(&mut self, attempt_id: u64, frame: &[u8]) -> Result<(), TransportError>;

    /// Close the transport and every lane on it.
    fn close(&mut self, attempt_id: u64, reason: &str);
}

/// The direct-carrier state machine, behind a name.
///
/// A host that owns several workers holds one of these per worker and never
/// reaches for the concrete type, which is what keeps "swap the WebRTC
/// implementation" a change to one file rather than a change to every caller.
pub trait PeerSignalling {
    /// Which worker this machine is one worker's.
    fn worker_fp(&self) -> &str;

    /// Where the attempt is, and what is on it.
    fn snapshot(&self) -> SignallingSnapshot;

    /// The attempt's current phase, for a host that logs transitions.
    fn phase(&self) -> PeerPhase;

    /// Fold one observation in, and return what the host should do about it.
    fn step(&mut self, input: SignallingInput) -> Vec<CarrierEffect>;
}

/// The test double: a machine driven by a script instead of by a transport.
///
/// It is a real implementation of the same trait and not a mock of the module, so
/// a host — or a front end that has no browser — can exercise the host's own
/// carrier wiring without a WebRTC stack anywhere in the process. That it can be
/// built at all is the point: the machine's contract is a value in and a value
/// out, with no transport reachable from either.
#[derive(Debug, Default)]
pub struct ScriptedPeerSignalling {
    worker_fp: String,
    inputs: Vec<SignallingInput>,
    scripted: Vec<CarrierEffect>,
    snapshot: SignallingSnapshot,
}

impl ScriptedPeerSignalling {
    /// A double for one worker, with an empty script.
    pub fn new(worker_fp: impl Into<String>) -> Self {
        let worker_fp: String = worker_fp.into();
        Self {
            snapshot: SignallingSnapshot::idle(worker_fp.clone()),
            worker_fp,
            inputs: Vec::new(),
            scripted: Vec::new(),
        }
    }

    /// What every `step` returns, in order, until the script runs out.
    pub fn script(&mut self, effects: Vec<CarrierEffect>) -> &mut Self {
        self.scripted = effects;
        self
    }

    /// Every input this double was driven with, in order.
    pub fn observed_inputs(&self) -> &[SignallingInput] {
        &self.inputs
    }

    /// Replace the snapshot this double reports, so a host can assert on it.
    pub fn set_snapshot(&mut self, snapshot: SignallingSnapshot) -> &mut Self {
        self.snapshot = snapshot;
        self
    }
}

impl PeerSignalling for ScriptedPeerSignalling {
    fn worker_fp(&self) -> &str {
        &self.worker_fp
    }

    fn snapshot(&self) -> SignallingSnapshot {
        self.snapshot.clone()
    }

    fn phase(&self) -> PeerPhase {
        self.snapshot.phase
    }

    fn step(&mut self, input: SignallingInput) -> Vec<CarrierEffect> {
        self.inputs.push(input);
        if self.scripted.is_empty() {
            return Vec::new();
        }
        vec![self.scripted.remove(0)]
    }
}
