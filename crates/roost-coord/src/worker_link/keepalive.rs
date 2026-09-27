//! The half-open detector: a socket that stops answering without closing.
//!
//! Owned by `worker_link::connection`. Pure — it is handed the clock, never
//! reads it — because the property worth testing is "a socket that went quiet
//! is closed rather than held open forever", and a test that has to wait real
//! time to observe that is a test that gets skipped.
//!
//! A TCP connection to a coordinator that has lost its route stays ESTABLISHED
//! on both ends: there is no RST, no FIN, and the worker believes it is linked
//! while its events accumulate unacknowledged in its outbox. Nothing else in
//! the read loop notices, because a quiet socket produces no frames. This is
//! the only thing that closes it.
//!
//! THE WINDOW IS SHARED WITH NOTHING and is deliberately longer than the
//! coordinator's own ping interval, so a link that is merely slow is not
//! declared dead between two pings.

use std::time::Duration;

/// How long a worker socket may be silent before it is treated as half-open.
///
/// 90 s, quoted from `protocol/spec/worker-link.md` §Limits
/// (`STALE_LINK_TIMEOUT_MS`). The coordinator pings well inside this, so a
/// silent socket is a dead route and not a quiet one.
pub const STALE_LINK_TIMEOUT: Duration = Duration::from_secs(90);

/// How often the read loop asks whether the socket is still alive.
///
/// 15 s (`STALE_LINK_CHECK_MS`). It is a quarter of the timeout so a socket is
/// never declared dead in the same tick it could have been seen alive.
pub const STALE_LINK_CHECK_INTERVAL: Duration = Duration::from_secs(15);

/// The read loop's own silence detector, over a clock it is handed.
#[derive(Debug, Clone, Copy)]
pub struct Keepalive {
    stale_after: Duration,
    last_activity: std::time::Instant,
}

impl Keepalive {
    /// A window at the contract's timeout, starting now.
    #[must_use]
    pub fn new(now: std::time::Instant) -> Self {
        Self::at(now, STALE_LINK_TIMEOUT)
    }

    /// A window at a timeout of the caller's choosing, for a test that must not
    /// sleep 90 seconds to observe a dead link.
    #[must_use]
    pub fn at(now: std::time::Instant, stale_after: Duration) -> Self {
        Self {
            stale_after,
            last_activity: now,
        }
    }

    /// Note that a frame arrived, which is the only thing that resets the
    /// window. An outbound ping does NOT: the question is whether the PEER is
    /// alive, and our own write says nothing about that.
    pub fn note_activity(&mut self, now: std::time::Instant) {
        self.last_activity = now;
    }

    /// Whether the socket has been silent past the timeout.
    #[must_use]
    pub fn is_stale(&self, now: std::time::Instant) -> bool {
        now.duration_since(self.last_activity) >= self.stale_after
    }

    /// How long the socket has been silent, for the close log line.
    #[must_use]
    pub fn silence(&self, now: std::time::Instant) -> Duration {
        now.saturating_duration_since(self.last_activity)
    }
}
