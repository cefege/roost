//! The complete input alphabet of the client: everything a host can tell the
//! core, and everything a front end can ask of it.
//!
//! One enum, so "what can this state machine react to?" has one answer a reader
//! can read top to bottom. It is closed on purpose: an open-ended input type is
//! how a state machine grows a branch nobody wrote a rule for, and the wire
//! frame vocabulary in `sync::inbound` is already where a new frame type belongs.
//!
//! Every variant is a FACT the host observed or an INTENT a front end expressed.
//! Nothing here is decoded from a wire format: the host owns decoding, so the
//! core's inputs are named and comparable.

use crate::effect::RpcResult;
use crate::search::RawMatch;
use crate::sync::inbound::SyncFrame;
use crate::terminal::input::InputOutcome;
use crate::terminal::routes::DirectCarrier;
use crate::terminal::token::TerminalToken;
use crate::terminal::view::ViewStateResult;

/// One thing that happened, or one thing that was asked for.
/// Only `PartialEq`: it carries wire types, which are `PartialEq` and not `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    // ---- transport observations ------------------------------------------------
    /// Open a Sync socket. The core names the handshake; the host owns the origin
    /// and the credential.
    ///
    /// An explicit event rather than something the constructor does, because the
    /// core may not do I/O — and a client whose connection happens as a side
    /// effect of being constructed cannot be constructed in a test without
    /// opening a socket.
    DialRequested,
    /// Fetch the bootstrap set: identity, sessions, workers.
    BootstrapRequested,
    /// A Sync socket is open and owns a coordinator scope.
    SyncLinkOpened {
        /// The dial this answers, and the generation it takes.
        generation: u64,
        /// The coordinator's identity for the socket.
        socket_id: String,
        /// The worker process epoch behind it.
        process_epoch: String,
    },
    /// The socket this generation owns is gone.
    SyncLinkClosed {
        /// The generation that closed.
        generation: u64,
        /// The WebSocket close code, when there was one. `4001` is the
        /// coordinator refusing the credential, which is terminal for the socket.
        close_code: Option<u16>,
    },
    /// One frame arrived on the Sync socket, already decoded.
    SyncFrameReceived {
        /// The generation it arrived on.
        generation: u64,
        /// Its transport sequence, or `0` for a control frame.
        delivery_seq: u64,
        /// The frame.
        frame: SyncFrame,
    },
    /// One frame arrived on a DIRECT carrier: loopback or WebRTC.
    ///
    /// A separate event from `SyncFrameReceived` because the routing differs. A
    /// cell frame on a direct carrier is folded into that carrier's STAGED
    /// replica, not into the session's canonical — a candidate with a
    /// half-built baseline must not be able to paint
    /// (`protocol/spec/direct-terminal.md:27`). A view-state or input result on a
    /// direct carrier is correlated by the carrier's own generation.
    DirectFrameReceived {
        /// The carrier generation the frame names. A frame whose token is not the
        /// one the candidate is folded on is refused without touching it.
        token: TerminalToken,
        /// The frame.
        frame: SyncFrame,
    },
    /// The pre-hydration store is ready, so retained frames may be applied.
    HydrationCompleted {
        /// The socket generation whose domains are now backed by a snapshot.
        generation: u64,
    },
    /// A Connect unary call answered.
    RpcResultReceived(RpcResult),
    /// The host signed a challenge.
    ChallengeSigned {
        /// The credential being established.
        account_id: String,
        /// The signature, opaque to the core.
        signature: Vec<u8>,
    },
    /// The credential is gone. Everything keyed to it is discarded, INCLUDING the
    /// recovery cursor: a persisted global cursor would skip the next socket's
    /// initial history.
    CredentialsDiscarded,

    // ---- front-end intent ------------------------------------------------------
    /// A pane attached to a session.
    ViewOpened {
        /// The session.
        session_id: String,
        /// The worker that owns the PTY.
        worker_fp: String,
        /// The pane's identity, stable for its life.
        view_id: String,
        /// The pane's effective columns.
        cols: u32,
        /// The pane's effective rows.
        rows: u32,
    },
    /// A pane changed size. The authority mints a new stream id for this, so the
    /// replica waits for a fresh baseline.
    ViewResized {
        /// The session.
        session_id: String,
        /// The pane.
        view_id: String,
        /// The new effective columns.
        cols: u32,
        /// The new effective rows.
        rows: u32,
    },
    /// A pane was hidden: it keeps its place but stops constraining geometry.
    ViewHidden {
        /// The session.
        session_id: String,
        /// The pane.
        view_id: String,
    },
    /// A pane closed, or its authorization was lost.
    ViewClosed {
        /// The session.
        session_id: String,
        /// The pane.
        view_id: String,
    },
    /// A generation-matched view-state result.
    ViewStateReceived {
        /// The generation the acknowledgement belongs to.
        generation: u64,
        /// The result.
        state: ViewStateResult,
    },
    /// Keystrokes from a pane. The core decides which route they go out on.
    TerminalInput {
        /// The session.
        session_id: String,
        /// The pane, when the keystroke came from one.
        view_id: Option<String>,
        /// The bytes to write.
        bytes: Vec<u8>,
    },
    /// A truthful write result for one admitted batch.
    InputResultReceived {
        /// The generation the result belongs to.
        generation: u64,
        /// The session.
        session_id: String,
        /// The batch's own sequence.
        input_seq: u64,
        /// What became of it.
        outcome: InputOutcome,
    },
    /// A direct carrier authenticated and is ready.
    CarrierReady(DirectCarrier),
    /// A direct carrier closed or was displaced.
    CarrierLost {
        /// The host's identity for the connection.
        connection_id: String,
    },
    /// A worker is gone: its routes, its demand, and its candidates all go.
    WorkerRetired {
        /// The worker fingerprint.
        worker_fp: String,
    },
    /// One coordinator scrollback-search page arrived.
    SearchPageReceived {
        /// The session searched.
        session_id: String,
        /// The window the coordinator says it read.
        page: crate::search::SearchPage,
        /// The matches it found, before they are fenced to an epoch.
        matches: Vec<RawMatch>,
        /// The row the reader's previous page ended at, when this continues one.
        before_row: Option<u32>,
    },

    // ---- time ------------------------------------------------------------------
    /// One pass over every deadline: the chunk stall, the resync retry, the view
    /// lease, the held-input timeout, the watermark write, the Sync liveness
    /// read. The core owns no timer, so this is how time reaches it — and a host
    /// that cannot run a timer cannot silently skip a deadline, because there is
    /// no other way one could fire.
    Sweep {
        /// The host's clock.
        now_ms: u64,
    },
}

impl ClientEvent {
    /// A short name for the incident log, so a host tracing an event does not
    /// have to match on the whole enum to say which one it was.
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::DialRequested => "dial_requested",
            Self::BootstrapRequested => "bootstrap_requested",
            Self::SyncLinkOpened { .. } => "sync_link_opened",
            Self::SyncLinkClosed { .. } => "sync_link_closed",
            Self::SyncFrameReceived { .. } => "sync_frame_received",
            Self::HydrationCompleted { .. } => "hydration_completed",
            Self::RpcResultReceived(_) => "rpc_result_received",
            Self::ChallengeSigned { .. } => "challenge_signed",
            Self::CredentialsDiscarded => "credentials_discarded",
            Self::DirectFrameReceived { .. } => "direct_frame_received",
            Self::ViewOpened { .. } => "view_opened",
            Self::ViewResized { .. } => "view_resized",
            Self::ViewHidden { .. } => "view_hidden",
            Self::ViewClosed { .. } => "view_closed",
            Self::ViewStateReceived { .. } => "view_state_received",
            Self::TerminalInput { .. } => "terminal_input",
            Self::InputResultReceived { .. } => "input_result_received",
            Self::CarrierReady(_) => "carrier_ready",
            Self::CarrierLost { .. } => "carrier_lost",
            Self::WorkerRetired { .. } => "worker_retired",
            Self::SearchPageReceived { .. } => "search_page_received",
            Self::Sweep { .. } => "sweep",
        }
    }
}
