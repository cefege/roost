//! Every way a worker socket can end, and the identity that ends with it.
//!
//! Owned by `worker_link::connection`, which is the only caller that closes a
//! socket. Split out so the close-code TABLE is one testable value rather than
//! six literals spread across a read loop, and because a close code is a
//! promise to the peer: the worker reconnects and replays on some of them and
//! gives up on others, so a wrong code is a data-loss bug rather than a logging
//! detail.
//!
//! THE WHOLE TABLE IS CONTRACT §7.6, and the reason it is written out rather
//! than derived is that each row answers a different question on the worker:
//! `1009` and `1008` are the worker's fault and it backs off, `4001` and `4003`
//! are the coordinator's and the worker must re-authenticate, and the no-code
//! default is "something threw, reconnect and replay what was never
//! acknowledged".

/// The `1009` close: the per-socket retained-work budget filled.
pub const CLOSE_QUEUE_OVERFLOW: u16 = 1009;

/// The `1008` close: 600 durable events inside 60 s, or a protocol violation.
pub const CLOSE_POLICY_VIOLATION: u16 = 1008;

/// The `4001` close: the key generation moved, or the key was revoked.
pub const CLOSE_REVOKED: u16 = 4001;

/// The `4003` close: the verified token's deadline passed.
pub const CLOSE_REAUTH_REQUIRED: u16 = 4003;

/// Why a worker socket ended, and what the peer is told.
///
/// `Close` is the whole vocabulary. `None` on the code is deliberate and means
/// "the socket died or a durable append threw" — the worker reconnects and
/// replays, and a code here would tell it to do something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketClose {
    /// The ordered frame queue's budget filled. `1009 worker queue overflow`.
    QueueOverflow,
    /// More than [`crate::worker_link::rate_window::DURABLE_EVENT_LIMIT`]
    /// durable events inside the window. `1008 worker event rate exceeded`.
    EventRateExceeded,
    /// A dedupe replay carried a different payload than the row it replayed.
    /// `1008`, because the peer's own framing is what is wrong.
    DedupeMismatch,
    /// The key was revoked or its generation moved. `4001 revoked`.
    Revoked,
    /// The token's deadline passed while the socket was open. `4003 reauth
    /// required`.
    ReauthRequired,
    /// A durable append threw, or the socket simply died. No code.
    Default,
}

impl SocketClose {
    /// The WebSocket close code, or `None` for the default close.
    ///
    /// `None` is the load-bearing half. A close code tells the peer what to do
    /// next, and every code in this table has an action attached to it; sending
    /// one for "the append threw" would tell the worker to re-authenticate or
    /// back off when the truth is that it should replay.
    #[must_use]
    pub const fn code(self) -> Option<u16> {
        match self {
            Self::QueueOverflow => Some(CLOSE_QUEUE_OVERFLOW),
            Self::EventRateExceeded | Self::DedupeMismatch => Some(CLOSE_POLICY_VIOLATION),
            Self::Revoked => Some(CLOSE_REVOKED),
            Self::ReauthRequired => Some(CLOSE_REAUTH_REQUIRED),
            Self::Default => None,
        }
    }

    /// The reason string, which is what an operator greps a worker log for.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::QueueOverflow => "worker queue overflow",
            Self::EventRateExceeded => "worker event rate exceeded",
            Self::DedupeMismatch => "protocol violation",
            Self::Revoked => "revoked",
            Self::ReauthRequired => "reauth required",
            Self::Default => "link closed",
        }
    }

    /// The wire close frame's reason, or `None` for the default close.
    #[must_use]
    pub const fn into_frame(self) -> Option<(u16, &'static str)> {
        match self.code() {
            Some(code) => Some((code, self.reason())),
            None => None,
        }
    }
}

/// One authenticated worker socket, and the per-socket state that dies with it.
///
/// Everything here is per-SOCKET, not per-worker: two sockets for the same
/// fingerprint have independent queues, windows and budgets, and sharing either
/// would let one connection's traffic exhaust another's. That is why the type
/// is constructed by the admission decision and handed down rather than looked
/// up from the worker registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketIdentity {
    /// The URL fingerprint, re-checked against the verified caller on every
    /// frame that can act on another worker's state.
    pub fingerprint: String,
    /// The key generation current when the token was verified. A frame that
    /// arrives under a moved generation closes the socket with
    /// [`SocketClose::Revoked`] rather than being served.
    pub key_generation: u64,
    /// The key's authorized label, for a log line and nothing else.
    pub label: String,
}

impl SocketIdentity {
    /// The identity of one admitted socket.
    #[must_use]
    pub fn new(fingerprint: String, key_generation: u64, label: String) -> Self {
        Self {
            fingerprint,
            key_generation,
            label,
        }
    }
}
