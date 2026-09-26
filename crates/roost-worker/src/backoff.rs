//! The coordinator link's reconnect policy: backoff, when a failed dial is
//! worth escalating, and when a link is stale enough to force-close.
//!
//! Owned by the worker. This module is deliberately PURE — no sockets, no
//! clock, no state that outlives a call. Every decision here was made from an
//! incident, and an incident you cannot reproduce in a unit test is an
//! incident you will get wrong again.
//!
//! THE PROBLEM THESE NUMBERS SOLVE, from 2026-08-01 on ovh1: a worker
//! throttled by its own cgroup escalated to the five-minute backoff cap after
//! three dials and then stayed invisible for minutes. The escalation was
//! working exactly as written and was still wrong, because it assumed a
//! specific cause for a symptom that has several.

use std::time::Duration;

/// The first backoff, before any streak is established.
pub const BACKOFF_INITIAL: Duration = Duration::from_millis(500);

/// The ceiling under normal circumstances.
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// The ceiling once a streak is judged to be auth rejections rather than a
/// transient stall.
pub const AUTH_REJECT_BACKOFF_CAP: Duration = Duration::from_secs(300);

/// Non-open dials tolerated before escalating, for a worker that has NEVER
/// opened a link in this process.
///
/// This is the real stale-binary case: a worker carrying a contract the
/// coordinator refuses will never open, and the honest response is to stop
/// dialling so hard that the operator notices.
pub const AUTH_REJECT_THRESHOLD: u32 = 3;

/// Non-open dials tolerated before escalating, once a link HAS opened.
///
/// Sixty, not three. A worker that has demonstrably worked is far more likely
/// to be meeting a transient stall or its own resource limits than a
/// rejected contract, and escalating on three turns a thirty-second blip into
/// five minutes of silence. The cost of this being too generous is a few extra
/// dials; the cost of it being too tight is the incident above.
pub const AUTH_REJECT_THRESHOLD_AFTER_OPEN: u32 = 60;

/// How long a link must stay up before the dial counters reset.
///
/// Without this, a `helloAck`-then-immediate-drop pattern cycles
/// `attempt: 1` forever, and a coordinator flapping every few seconds looks
/// like a worker that has never connected at all. That hides the pathology
/// from the telemetry that would otherwise show it.
pub const STABLE_SESSION: Duration = Duration::from_secs(30);

/// A healthy open link receives a downstream frame at least this often, so a
/// link silent for this long is not merely idle.
pub const STALE_LINK_TIMEOUT: Duration = Duration::from_secs(90);

/// How often the watchdog checks.
pub const STALE_CHECK_INTERVAL: Duration = Duration::from_secs(15);

/// The backoff ceiling for a run of `non_open_streak` dials that never opened.
///
/// `has_opened` is the whole decision. A worker that has never opened is
/// probably carrying something the coordinator will not accept; a worker that
/// has opened is probably fine and the link is just having a bad time.
pub fn backoff_cap(non_open_streak: u32, has_opened: bool) -> Duration {
    let threshold = if has_opened {
        AUTH_REJECT_THRESHOLD_AFTER_OPEN
    } else {
        AUTH_REJECT_THRESHOLD
    };
    if non_open_streak >= threshold {
        AUTH_REJECT_BACKOFF_CAP
    } else {
        BACKOFF_MAX
    }
}

/// The delay before dial number `attempt` (1-based).
///
/// Geometric from [`BACKOFF_INITIAL`], saturating at the cap, so a short blip
/// retries quickly and a long one stops hammering.
pub fn backoff_delay(attempt: u32, non_open_streak: u32, has_opened: bool) -> Duration {
    let cap = backoff_cap(non_open_streak, has_opened);
    if attempt <= 1 {
        return BACKOFF_INITIAL.min(cap);
    }
    // `saturating_mul` on the exponent: an attempt count from a long-running
    // flapping coordinator must not overflow into a SHORT delay, which is the
    // one thing backoff must never do.
    let factor = 1u32
        .checked_shl(attempt.saturating_sub(1).min(31))
        .unwrap_or(u32::MAX);
    BACKOFF_INITIAL.saturating_mul(factor).min(cap)
}

/// What a link's recent traffic says about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkHealth {
    /// Whether this process has ever had a link open.
    pub has_opened: bool,
    /// How long the current link has been up, if it is up.
    pub uptime: Option<Duration>,
    /// How long since the last frame arrived downstream.
    pub since_last_frame: Duration,
    /// Dials since the last one that opened.
    pub non_open_streak: u32,
    /// The next attempt number.
    pub attempt: u32,
}

impl LinkHealth {
    pub fn new() -> Self {
        Self::default()
    }

    /// A link that has just opened.
    pub fn opened(now_uptime: Duration) -> Self {
        Self {
            has_opened: true,
            uptime: Some(now_uptime),
            since_last_frame: Duration::ZERO,
            non_open_streak: 0,
            attempt: 1,
        }
    }

    /// Whether the link is stale enough to force-close and re-dial.
    ///
    /// A healthy open link never goes [`STALE_LINK_TIMEOUT`] without a
    /// downstream frame, so silence that long is not idleness. This is the
    /// half-open-through-a-proxy case: the coordinator process died behind its
    /// front door, the worker-side connection stays ESTABLISHED, and every send
    /// "succeeds" into a black hole — no error, no close, no signal. On
    /// 2026-07-11 that ran for seven hours with every spawn failing
    /// `[failed_precondition] worker not connected`.
    pub fn is_stale(&self) -> bool {
        self.has_opened && self.uptime.is_some() && self.since_last_frame >= STALE_LINK_TIMEOUT
    }

    /// Whether the dial counters should reset.
    ///
    /// Only after a link has stayed up long enough to count as working. A
    /// link that opens and immediately drops has not proved anything, and
    /// resetting on it would cycle `attempt: 1` forever and hide a flapping
    /// coordinator.
    pub fn should_reset_counters(&self) -> bool {
        self.uptime.is_some_and(|uptime| uptime >= STABLE_SESSION)
    }

    /// Record a dial that never opened.
    pub fn record_non_open(&mut self) {
        self.non_open_streak = self.non_open_streak.saturating_add(1);
        self.attempt = self.attempt.saturating_add(1);
        self.uptime = None;
    }

    /// The delay before the next dial.
    pub fn next_delay(&self) -> Duration {
        backoff_delay(self.attempt, self.non_open_streak, self.has_opened)
    }
}
