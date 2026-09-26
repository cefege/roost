//! The delivery surface a terminal view host must provide, and the two value
//! types the membership machine hands back instead of calling out.
//!
//! Ported from `packages/protocol/src/terminal-view/screen-port.ts` and the
//! `TerminalViewStateSink` half of `terminal-view-protocol.ts`, plus the three
//! `TerminalScreenHub` methods the owner relay drives. The Sync socket driver
//! implements it.
//!
//! WHY THE MACHINE RETURNS EFFECTS INSTEAD OF CALLING THEM. The recompute hook
//! is the obvious case: it reads the registry to recompute, so a registry that
//! called it under its own lock would self-deadlock. Making every outbound
//! effect a returned value keeps one rule -- the registry never calls out -- and
//! the hub performs the effects after the guard is dropped.

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{FirehoseFrame, TerminalViewStateFrame, TerminalViewStatus};
use roost_protocol::wire::SessionId;

/// The screen delivery surface a terminal view host provides.
///
/// The hub calls exactly these when membership, watch state, an owner stream or
/// a client checkpoint changes; the host decides how a baseline or a delta
/// reaches the socket.
pub trait TerminalViewSink: Send + Sync {
    /// Queue one view-state frame for a socket, stamped for the session lane.
    fn enqueue_terminal_state(&self, socket_id: &str, frame: FirehoseFrame, session_id: &str);

    /// The socket is now watching this session, or no longer is.
    fn set_watching(&self, socket_id: &str, session_id: &SessionId, watching: bool);

    /// Serve the socket a baseline for the session's current stream. `true`
    /// when a baseline was actually queued.
    fn seed_socket(&self, socket_id: &str, session_id: &SessionId) -> bool;

    /// Serve the socket forward from its own checkpoint, or force a fresh
    /// baseline when the host cannot prove the checkpoint is still reachable.
    fn resync_socket(&self, socket_id: &str, session_id: &SessionId, grid_epoch: &str, seq: u64);

    /// A live view's lease lapsed. The host is expected to close that socket:
    /// its other sessions' views are parked by the close, and a socket that
    /// stopped heartbeating is not a socket to keep.
    fn live_view_expired(&self, socket_id: &str, view_id: &str, session_id: &SessionId);

    /// The session's stream is `stream_id` at this geometry. A stream change
    /// resets the replica, which is why the hub reads the previous stream id
    /// before it installs the new one.
    fn expect_stream(&self, session_id: &SessionId, stream_id: &str, cols: u32, rows: u32);

    /// The stream the session's replica is already expecting.
    fn expected_stream_id(&self, session_id: &SessionId) -> Option<String>;

    /// The replica cannot serve this session from cache, so a fresh full is owed.
    fn invalidate(&self, session_id: &SessionId, reason: &str);
}

/// The longest reason a view-state frame carries, in bytes.
///
/// The client renders this string, and a refusal reason that quoted a whole
/// command would otherwise be the largest frame on the wire.
pub const VIEW_REASON_MAX_BYTES: usize = 200;

/// A reason clipped to [`VIEW_REASON_MAX_BYTES`] on a character boundary, so a
/// clipped reason stays readable rather than ending mid-UTF-8.
#[must_use]
pub fn truncate_view_reason(value: &str) -> String {
    if value.len() <= VIEW_REASON_MAX_BYTES {
        return value.to_owned();
    }
    let mut end = VIEW_REASON_MAX_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// The view-state frame a view decision answers with.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn view_state_frame(
    view_id: &str,
    session_id: &str,
    revision: u64,
    active: bool,
    stream_id: &str,
    status: TerminalViewStatus,
    effective_cols: u32,
    effective_rows: u32,
    reason: &str,
) -> FirehoseFrame {
    FirehoseFrame {
        frame: Some(Frame::TerminalViewState(Box::new(TerminalViewStateFrame {
            view_id: view_id.to_owned(),
            session_id: session_id.to_owned(),
            revision,
            active,
            stream_id: stream_id.to_owned(),
            status: status.into(),
            effective_cols,
            effective_rows,
            reason: truncate_view_reason(reason),
            __buffa_unknown_fields: Default::default(),
        }))),
        ..FirehoseFrame::default()
    }
}

/// One answer the membership machine owes a socket, resolved against the
/// session's effective geometry AFTER the recompute that this decision caused.
///
/// The geometry is deliberately not filled in here: the machine decides
/// membership, the hub recomputes, and only then does a record reply have a
/// geometry to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingReply {
    /// Answer the command on the socket that sent it. Carries no geometry,
    /// because a command the machine refused, or a view it released, has none.
    Command {
        /// The socket the frame goes to.
        socket_id: String,
        /// The view id the client minted.
        view_id: String,
        /// The session the command named.
        session_id: String,
        /// The revision the client declared.
        revision: u64,
        /// Whether the client still wants frames.
        active: bool,
        /// The decision.
        status: TerminalViewStatus,
        /// Why, when the decision was not an acceptance.
        reason: String,
    },
    /// Answer a view record, whose effective geometry the hub fills in.
    View {
        /// The socket the frame goes to.
        socket_id: String,
        /// The record's view id.
        view_id: String,
        /// The session the record watches.
        session_id: String,
        /// The revision the record holds.
        revision: u64,
        /// The decision.
        status: TerminalViewStatus,
        /// Why, when the decision was not an acceptance.
        reason: String,
    },
}

impl PendingReply {
    /// The socket this reply goes to.
    #[must_use]
    pub fn socket_id(&self) -> &str {
        match self {
            Self::Command { socket_id, .. } | Self::View { socket_id, .. } => socket_id,
        }
    }

    /// The session this reply is stamped for.
    #[must_use]
    pub fn session_id(&self) -> &str {
        match self {
            Self::Command { session_id, .. } | Self::View { session_id, .. } => session_id,
        }
    }
}

/// One host-side effect the membership machine decided, performed by the hub
/// after it has released the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkCall {
    /// The socket's watch state for this session changed.
    Watching {
        /// The socket.
        socket_id: String,
        /// The session.
        session_id: SessionId,
        /// Whether it still holds a view of it.
        watching: bool,
    },
    /// A client asked to be served forward from its own checkpoint.
    Resync {
        /// The socket.
        socket_id: String,
        /// The session.
        session_id: SessionId,
        /// The grid epoch the checkpoint was taken in.
        grid_epoch: String,
        /// The sequence the checkpoint was taken at.
        seq: u64,
    },
    /// A live view's lease lapsed and the host is expected to close its socket.
    LiveViewExpired {
        /// The socket that stopped heartbeating.
        socket_id: String,
        /// The view whose lease lapsed.
        view_id: String,
        /// The session it watched.
        session_id: SessionId,
    },
}

impl SinkCall {
    /// The session this effect is about, for the one log line a sweep writes.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        match self {
            Self::Watching { session_id, .. }
            | Self::Resync { session_id, .. }
            | Self::LiveViewExpired { session_id, .. } => session_id,
        }
    }
}

/// A sink that answers every call with "nothing to do".
///
/// A coordinator with no Sync socket -- a test, or a boot that has not reached
/// the socket driver -- has no socket to deliver to, and pretending otherwise
/// would put a frame somewhere no client can read it.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTerminalViewSink;

impl TerminalViewSink for NoTerminalViewSink {
    fn enqueue_terminal_state(&self, _socket_id: &str, _frame: FirehoseFrame, _session_id: &str) {}

    fn set_watching(&self, _socket_id: &str, _session_id: &SessionId, _watching: bool) {}

    fn seed_socket(&self, _socket_id: &str, _session_id: &SessionId) -> bool {
        false
    }

    fn resync_socket(
        &self,
        _socket_id: &str,
        _session_id: &SessionId,
        _grid_epoch: &str,
        _seq: u64,
    ) {
    }

    fn live_view_expired(&self, _socket_id: &str, _view_id: &str, _session_id: &SessionId) {}

    fn expect_stream(&self, _session_id: &SessionId, _stream_id: &str, _cols: u32, _rows: u32) {}

    fn expected_stream_id(&self, _session_id: &SessionId) -> Option<String> {
        None
    }

    fn invalidate(&self, _session_id: &SessionId, _reason: &str) {}
}
