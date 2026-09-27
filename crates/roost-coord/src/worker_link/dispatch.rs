//! The contract between the worker socket and whatever a frame turns into.
//!
//! Owned by `worker_link::connection`, which is the only caller, and
//! implemented by `worker_link::frame_dispatch`, which owns the three arms.
//! The trait is HERE and the implementation is THERE on purpose, and the reason
//! is worth stating because it is not a layering preference:
//! `serve_socket` must dispatch every frame it reads, so the read loop and the
//! dispatcher are one behaviour split across two files rather than two
//! features that happen to touch a socket. Putting the contract on this side
//! means the read loop can be written and tested against a real signature, and
//! putting the implementation on the other means neither half is a stub.
//!
//! WHAT A FRAME CAN BE, per contract §3.3, and why the classes are not one
//! enum here: the durable arm has an ACKNOWLEDGEMENT and a `client_seq` and
//! may close the socket; the live arms have neither and may not. Collapsing
//! them would make "does this frame need an ack?" a runtime question on a value
//! that should answer it at compile time.

use std::pin::Pin;

use connectrpc::ConnectError;

use crate::worker_link::conn_types::SocketClose;

/// What a frame handler decided, which is the only thing the read loop may act
/// on. It cannot close the socket by itself; it REQUESTS a close and the loop
/// performs it, so there is exactly one place a close frame is written and one
/// place it is logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// Handled. The frame's bytes are released once this returns.
    Handled,
    /// Refused with a reason that does not close the link — a malformed frame
    /// the worker can correct by resending. The frame is still released.
    Refused,
    /// The link must end, and this is why. The queue is latched first so no
    /// further frame can be charged while the close is in flight.
    Close(SocketClose),
}

/// Which of the three arms a frame belongs to, decided by the read loop from
/// the frame header and NOT by the dispatcher.
///
/// The class is the whole reason this file exists, so it is worth being blunt
/// about what it buys: only [`FrameClass::Durable`] is genuinely asynchronous
/// (`EventLog::append_event` awaits the database), and the two live classes
/// resolve to synchronous state. A single boxed-future method for all three
/// would put a heap allocation on the path that carries every PTY byte a
/// machine produces, to serve a `dyn` the other two arms never need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameClass {
    /// A durable `SessionEvent`. Awaited, and acknowledged one at a time.
    Durable,
    /// Terminal bytes and state, forwarded to the byte hub, views and buses.
    /// Synchronous: no await, and no allocation.
    Live,
    /// An `rpc-ok` or `rpc-error` answering a request the coordinator is
    /// holding open. Synchronous, into the pending-RPC table.
    Rpc,
}

impl FrameClass {
    /// Whether this class's handler is asynchronous, and so whether dispatching
    /// it costs an allocation.
    #[must_use]
    pub const fn is_async(self) -> bool {
        matches!(self, Self::Durable)
    }
}

/// One frame, and the socket it arrived on.
///
/// `channel` is the frame's own channel id and is NOT trusted: the dispatcher
/// re-checks it against the durable route before acting on another worker's
/// state, which is what `mapping_mismatch` is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundFrame {
    /// Which arm this frame belongs to.
    pub class: FrameClass,
    /// The frame's declared channel, as the peer wrote it.
    pub channel: u32,
    /// The frame's payload, exactly as it arrived off the wire.
    pub payload: Vec<u8>,
}

/// What one worker socket's frames turn into.
///
/// One implementation, on `CoordServices`, so both sockets — the worker's and
/// the Sync socket's — resolve the same dispatcher rather than each having its
/// own.
pub trait FrameDispatch: Send + Sync {
    /// Handle one SYNCHRONOUS frame — a live or an rpc one.
    ///
    /// Not boxed, and not async, because neither of those arms awaits
    /// anything. This is the hot path: it carries every PTY byte a machine
    /// produces, and a `Pin<Box<dyn Future>>` here would be one heap
    /// allocation per frame to serve a dynamic dispatch the compiler can do
    /// statically inside this crate.
    fn handle_now(&self, worker_fp: &str, frame: &InboundFrame) -> DispatchOutcome;

    /// Handle one DURABLE frame, which awaits the database and is
    /// acknowledged one at a time.
    ///
    /// The `&mut` is not an oversight: a durable frame's `client_seq` is
    /// acknowledged ONE AT A TIME, so the handler owns a cursor this socket
    /// advances. Taking `&self` would force a second lock inside the handler
    /// and put the "one at a time" property somewhere it cannot be seen.
    ///
    /// The boxed future is the price of `dyn`, paid on the durable arm only.
    /// Durable frames are `SessionEvent`s — orders of magnitude fewer than
    /// the byte frames above — so the allocation lands where the await already
    /// is rather than on the path that does not need it.
    fn handle_durable<'a>(
        &'a mut self,
        worker_fp: &'a str,
        frame: &'a InboundFrame,
    ) -> Pin<DispatchFuture<'a>>;
}

/// The boxed future a [`FrameDispatch`] returns.
///
/// A `Future` rather than an `async fn` in the trait so the trait stays
/// object-safe: `CoordServices` holds a `dyn FrameDispatch`, and an `async fn`
/// in a trait cannot be called through one.
pub type DispatchFuture<'a> = Pin<Box<dyn Future<Output = DispatchOutcome> + Send + 'a>>;

/// How a frame's bytes are accounted once a handler has settled.
///
/// `release` is separate from `handle` on purpose and mirrors
/// [`crate::worker_link::frame_queue::FrameQueue::release`]: the budget stays
/// charged for the whole time the handler runs, so a slow handler is visible to
/// the bound rather than free.
#[derive(Debug)]
pub struct ChargedFrame<'a> {
    /// The frame that was charged.
    pub frame: &'a InboundFrame,
    /// The bytes to give back, handed to the queue.
    pub charged_bytes: usize,
}

impl ChargedFrame<'_> {
    /// Give the frame's bytes back.
    #[must_use]
    pub fn release(self) -> usize {
        self.charged_bytes
    }
}

/// The refusal a handler returns for a frame it will not process at all, and
/// the close that follows from one it cannot recover from.
///
/// Split because they are different promises to the peer: one says "resend
/// that", the other says "stop". v2's transport is explicit about the
/// distinction and collapsing it is how a worker ends up in a reconnect loop
/// against a coordinator that is merely confused by one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRefusal {
    /// A dedupe replay carried a different payload than the row it replayed.
    /// Closes `1008`: the peer's own framing is wrong.
    DedupeMismatch,
    /// The key generation moved under the socket.
    Revoked,
    /// The token's deadline passed while the socket was open.
    ReauthRequired,
}

impl FrameRefusal {
    /// The close this refusal earns.
    #[must_use]
    pub const fn close(self) -> SocketClose {
        match self {
            Self::DedupeMismatch => SocketClose::DedupeMismatch,
            Self::Revoked => SocketClose::Revoked,
            Self::ReauthRequired => SocketClose::ReauthRequired,
        }
    }
}

/// The error a durable append returns, and what the socket does with it.
///
/// The no-code default is the whole point: an append that threw leaves a HOLE
/// the worker cannot know about, so the only honest answer is to close without
/// a code so the worker reconnects and replays everything unacknowledged.
pub fn close_for_append_error(_error: &ConnectError) -> SocketClose {
    SocketClose::Default
}
