//! The direct carrier's vocabulary: the attempt, the messages that reach it,
//! what it asks the host to do, and the types a host reads it through. Called
//! through `PeerSignalling` and `PeerTransport`; the rules live in the
//! submodules, of which `loopback` holds the gate that keeps a WebRTC peer from
//! being the default carrier, and `faults` the four rules
//! `smoke/terminal/terminal-peer.spec.ts:263` names.

pub mod deliver;
pub mod faults;
pub mod grant;
pub mod grant_rpc;
pub mod inbound;
pub mod lane;
pub mod loopback;
pub mod probe_state;
pub mod signaling;
mod signaling_demand;
mod signaling_open;
pub mod signaling_snapshot;
pub mod transport_trait;
pub mod wire;

pub use deliver::{CarrierPresence, Delivery, SendFault, deliver_direct_command, session_of};
pub use faults::{
    CarrierFault, FallbackReason, FaultFallback, FaultState, PEER_HOLD_DOWN_MS, answer_fault,
    classify_worker_reason, fallback_effects, ready_fault, retry_delay_ms, sdp_is_usable,
};
pub use grant::{
    DirectGrant, GRANT_RENEW_MS, GRANT_RETRY_MS, GrantInput, GrantLifecycle, GrantPhase, GrantSweep,
};
pub use inbound::{DirectInbound, DirectScrollback, DirectTerminalImage};
pub use lane::CarrierLane;
pub use loopback::{LOOPBACK_GRACE_MS, LocalWorkerDoor, LoopbackAnswer, LoopbackProbe};
pub use probe_state::{ProbeReading, TransportProbeState};
pub use signaling::Signalling;
pub use signaling_snapshot::{
    CandidateType, DirectPhaseTimings, PeerTelemetry, SignallingSnapshot,
};
pub use transport_trait::{
    PeerLane, PeerSignalling, PeerTransport, ScriptedPeerSignalling, TransportError,
};
pub use wire::{
    WireError, decode_server_frame, encode_direct_command, encode_hello, encode_transport_probe,
    peer_ready_tuple,
};

use std::collections::BTreeSet;

use crate::effect::Effect;
use crate::terminal::routes::DirectCarrier;
use crate::terminal::token::{TerminalToken, TerminalTransport};

// The three lane labels the protocol fixes, not a spelling decided here: the
// worker's peer and this browser's peer must open the same three channels, or
// SCTP will not pair their streams.
pub use roost_protocol::versioning::{
    CHANNEL_TERMINAL_CONTROL_V1, CHANNEL_TERMINAL_DATA_V1, CHANNEL_TERMINAL_HISTORY_V1,
};

/// Where one worker's direct-carrier attempt is. The order is the
/// negotiation's own — open, offer, answer, authenticate, then hold — and
/// `Cooldown` and `Disabled` are the two ways an attempt stops, differing in
/// whether the machine will come back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PeerPhase {
    /// Nothing in flight, and the probe has not released a peer.
    #[default]
    Idle,
    /// Waiting on a grant, with or without a transport gathering for it.
    AwaitingGrant,
    /// The transport is open and ICE is gathering.
    Gathering,
    /// The offer is with the coordinator.
    Negotiating,
    /// The answer is applied and the carrier is proving its tuple.
    Authenticating,
    /// Authenticated and staged, not yet elected.
    Candidate,
    /// A promotion committed on this carrier.
    Active,
    /// Failed, and waiting out a retry.
    Cooldown,
    /// This document or this worker will never do WebRTC. Parked, not retried.
    Disabled,
}

/// What the host can tell one worker's machine once, at construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarrierEnvironment {
    /// How many WebRTC peers this document already holds, across every worker.
    /// Document-wide, not per worker: eight peers each times a visible fleet is
    /// how a tab runs a browser out of descriptors.
    pub peers_allocated: u32,
    /// Whether the document can do WebRTC at all: a secure context that
    /// exposes a peer constructor. A `false` here PARKS the machine rather than
    /// retrying, because nothing about the document will change.
    pub peer_transport_available: bool,
    /// The Sync generation this worker's sessions are fenced to, carried to the
    /// snapshot so "Sync metadata stayed live" is readable rather than assumed.
    pub sync_generation: u64,
    /// STUN servers the coordinator advertised; `None` until it answered, which
    /// keeps the attempt behind the grant. `Some(empty)` gathers host only.
    pub stun_urls: Option<Vec<String>>,
}

/// One attempt at a direct carrier for one worker. An attempt is IDENTIFIED by
/// `(worker_fp, worker_epoch, peer_id)`, and every later message is checked
/// against those three: one naming any other tuple is not this attempt's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAttempt {
    /// Monotonic per worker, so a late answer for a dead attempt is discardable.
    pub attempt_id: u64,
    /// The worker this attempt reaches.
    pub worker_fp: String,
    /// The coordinator's identity for the worker PROCESS. A restart changes it,
    /// which is what makes a peer from a dead attempt unusable.
    pub worker_epoch: String,
    /// Loopback or WebRTC.
    pub transport: TerminalTransport,
    /// Opaque, browser-allocated, and the third half of the tuple a `Ready` has
    /// to match. Names THIS negotiation and nothing else. The host mints it as
    /// it opens the transport and reports it with the offer, so it is empty
    /// until `OfferReady`; nothing reads it before then.
    pub peer_id: String,
    /// The grant this attempt authenticates with; empty until a mint is adopted.
    pub grant_id: String,
    /// The tab the grant names.
    pub tab_id: String,
    /// The device the grant names.
    pub device_fingerprint: String,
    /// Opportunistic address discovery only. Empty disables it, and it is never
    /// a relay guarantee.
    pub stun_urls: Vec<String>,
    /// The exact sessions this attempt may carry.
    pub session_ids: BTreeSet<String>,
}

/// The identity a far end proves in its `Ready`, before it may present a token.
/// `domain_generation` is not a field because the protocol carries no separate
/// one: the socket generation IS it, and two spellings of one number would be a
/// chance to disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyTuple {
    /// The worker it claims to be.
    pub worker_fp: String,
    /// The worker process epoch it claims.
    pub worker_epoch: String,
    /// The peer id it was allocated.
    pub peer_id: String,
    /// The coordinator's socket generation for this link. Zero is refused: no
    /// generation is not the first generation.
    pub socket_generation: u64,
    /// The coordinator's socket id. Never empty: a `Ready` without one names no
    /// authority to fence against.
    pub socket_id: String,
    /// The sessions it will carry. A superset of the grant's scope is refused
    /// even when the rest of the tuple matches.
    pub session_ids: BTreeSet<String>,
}

impl ReadyTuple {
    /// Whether this is the attempt's own tuple, inside the attempt's own grant.
    pub fn admits(&self, attempt: &PeerAttempt) -> bool {
        self.worker_fp == attempt.worker_fp
            && self.worker_epoch == attempt.worker_epoch
            && self.peer_id == attempt.peer_id
            && self.socket_generation > 0
            && !self.socket_id.is_empty()
            && self.session_ids.is_subset(&attempt.session_ids)
    }

    /// The token this carrier would present, once the tuple has been admitted.
    /// Only ever called on an admitted tuple: a token minted from a refused one
    /// is a generation nothing else in the client has ever seen.
    pub fn carrier(&self, attempt: &PeerAttempt, connection_id: String) -> DirectCarrier {
        DirectCarrier {
            connection_id,
            worker_fp: self.worker_fp.clone(),
            transport: attempt.transport,
            token: TerminalToken::direct(
                self.socket_generation,
                attempt.transport,
                self.worker_fp.clone(),
                self.worker_epoch.clone(),
                self.socket_generation,
            ),
            socket_id: self.socket_id.clone(),
            granted_sessions: self.session_ids.clone(),
        }
    }
}

/// The coordinator's answer, exactly as it arrived: carried whole so the tuple
/// check is one comparison and not three fields unwrapped at each use site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAnswer {
    /// The peer id the coordinator says it bound the offer to.
    pub peer_id: String,
    /// The worker epoch the coordinator says it bound the offer to.
    pub worker_epoch: String,
    /// The worker's answer, undecoded. Admitted by `roost_protocol`'s SDP
    /// inspector, deliberately NOT by the tuple check.
    pub answer_sdp: String,
}

impl PeerAnswer {
    /// Whether the coordinator's answer is for THIS attempt.
    ///
    /// The SDP is not part of this: an SDP that does not parse is a different
    /// fault from a tuple that does not match, and the two are reported apart.
    pub fn binds(&self, attempt: &PeerAttempt) -> bool {
        self.peer_id == attempt.peer_id && self.worker_epoch == attempt.worker_epoch
    }
}

/// One thing the direct-carrier machine observed, or one thing it was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignallingInput {
    /// A view started or stopped wanting a session on this worker.
    Demand {
        /// The session.
        session_id: String,
        /// The view.
        view_id: String,
        /// Whether it is active.
        active: bool,
        /// The host's clock, which is when a direct-route wait starts.
        now_ms: u64,
    },
    /// Hold a granted peer ready on this worker for these sessions though no
    /// view wants one yet; an empty set stops pre-warming and keeps whatever
    /// peer it brought up.
    Prewarm {
        /// The sessions the grant is held for.
        session_ids: BTreeSet<String>,
        /// The host's clock.
        now_ms: u64,
    },
    /// Pre-warm no longer selects this worker: stop pre-warming it, and close
    /// its peer when no view wants it, which frees that peer's slot under the
    /// document's cap.
    PrewarmReleased {
        /// The host's clock.
        now_ms: u64,
    },
    /// The grant changed, or a request for one resolved.
    Grant(GrantInput),
    /// The loopback probe learned which worker this page's own machine runs, or
    /// empty for "not a worker's machine".
    LocalDoorAnswered {
        /// The worker's own fingerprint, or empty.
        worker_fp: String,
    },
    /// A loopback carrier for this worker came up or went down.
    ///
    /// The carrier itself belongs to the loopback slice, and this is the
    /// ELECTION's whole view of it: a staged one is the fallback a faulted peer
    /// hands the session to, and an unstaged one is the signal to look for a
    /// replacement. Nothing here opens or closes the socket.
    LoopbackCarrierStaged {
        /// Whether one is staged right now.
        staged: bool,
    },
    /// The transport opened its lanes and the offer SDP is ready, already
    /// candidate-filtered by the transport.
    OfferReady {
        /// Which attempt.
        attempt_id: u64,
        /// The peer id the host minted for this attempt's transport.
        peer_id: String,
        /// The local description.
        offer_sdp: String,
    },
    /// The coordinator answered.
    AnswerReceived {
        /// Which attempt.
        attempt_id: u64,
        /// The answer.
        answer: PeerAnswer,
    },
    /// A peer allocation refused the offer, the coordinator could not be
    /// reached, or the transport closed before authenticating. `None` names no
    /// attempt, which is the coordinator case: no offer was ever answered.
    AttemptRefused {
        /// Which attempt, when one was named.
        attempt_id: Option<u64>,
        /// The worker's own reason, from the eight the protocol fixes.
        reason: String,
    },
    /// The far end proved its tuple.
    PeerAuthenticated {
        /// Which attempt.
        attempt_id: u64,
        /// What it proved.
        ready: ReadyTuple,
    },
    /// The transport's ICE connection failed.
    IceFailed {
        /// Which attempt.
        attempt_id: u64,
    },
    /// The host's unanswered-probe rule has fired for this attempt.
    ProbeMissed {
        /// Which attempt.
        attempt_id: u64,
    },
    /// A promotion committed.
    PromotionCommitted {
        /// The session.
        session_id: String,
        /// The exact generation it committed on.
        token: TerminalToken,
        /// The host's clock, which is when a direct-route wait ends.
        now_ms: u64,
    },
    /// The host's clock reached the retry this machine asked for.
    RetryDue {
        /// The host's clock.
        now_ms: u64,
    },
    /// One pass over this worker's deadlines.
    Sweep {
        /// The host's clock.
        now_ms: u64,
    },
    /// This worker was removed: its routes, its demand, and its grant all go.
    WorkerRetired,
}

/// One thing the host should do about a direct carrier, now. `Core` carries an
/// `Effect` VERBATIM so this machine cannot grow a second vocabulary for a
/// decision the client core already speaks; the rest are the peer-lifecycle
/// actions only a transport can perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarrierEffect {
    /// A decision the client core already speaks.
    Core(Effect),
    /// Open this attempt's transport: the three ordered data channels for a
    /// peer, or the worker's loopback socket.
    OpenTransport {
        /// The attempt to open.
        attempt: PeerAttempt,
    },
    /// Send this attempt's offer to the coordinator. The coordinator is the
    /// signalling authority, so an offer never goes to another peer.
    NegotiateOffer {
        /// The attempt, carrying the grant it adopted while it gathered.
        attempt: PeerAttempt,
        /// The local offer.
        offer_sdp: String,
    },
    /// Hand the coordinator's answer to the open transport.
    ApplyAnswer {
        /// Which attempt.
        attempt_id: u64,
        /// The remote answer.
        answer_sdp: String,
    },
    /// The carrier proved its tuple; stage it as a candidate. The
    /// `DirectCarrier` is the HOST's to build, because the connection id is the
    /// host's to mint. Staged is NOT elected: a candidate with no complete
    /// validated baseline has nothing canonical to say.
    StageCarrier {
        /// The attempt that proved the tuple.
        attempt_id: u64,
        /// What it proved. Already admitted against the attempt.
        ready: ReadyTuple,
    },
    /// This attempt is over. Close whatever it opened.
    CloseAttempt {
        /// Which attempt.
        attempt_id: u64,
        /// Why, for the host's own log.
        reason: String,
    },
    /// The attempt failed, and this session is now served by the other
    /// transport.
    Fallback {
        /// The session that lost its direct route.
        session_id: String,
        /// Who is serving it now.
        transport: FaultFallback,
    },
    /// The attempt failed. Diagnostic only: the `Fallback` above is the
    /// decision, and this is the line that says which rule fired.
    Faulted {
        /// Which worker.
        worker_fp: String,
        /// What went wrong.
        fault: CarrierFault,
        /// The host's own detail, never the value that failed to match.
        detail: String,
    },
    /// Come back at this time. The core owns no timer, so the host schedules the
    /// retry and reports it as `SignallingInput::RetryDue`.
    RetryAt {
        /// The host's clock value to come back at.
        at_ms: u64,
    },
}
