//! When to dial again, and when to say out loud that the worker is wedged.
//! Called by the link loop once per dial and once per dial's end.
//!
//! Every number this file applies comes from [`crate::backoff`], which is
//! where the incidents behind them are written down. What lives here is the
//! bookkeeping that turns those numbers into a ladder: which of the three
//! things that can happen to a dial — it never opened, it opened and proved
//! itself, it opened and immediately dropped — feeds the counters.
//!
//! The third case is the one this file exists for. A coordinator that accepts
//! a socket and closes it two seconds later resets every counter if "opened" is
//! read as "succeeded", so the ladder never grows, the worker redials every
//! 500ms forever, and the one signal that would explain it — an escalated
//! backoff — never fires. A link that has not been up for
//! [`STABLE_SESSION`](crate::backoff::STABLE_SESSION) has not proved anything,
//! so it counts as a non-open dial.

use std::time::{Duration, Instant};

use crate::backoff::{
    AUTH_REJECT_BACKOFF_CAP, LinkHealth, STABLE_SESSION, STALE_CHECK_INTERVAL, backoff_cap,
};

/// A streak that has just crossed into the escalated ceiling.
///
/// Reported once, at the crossing, because the ceiling itself is already in the
/// delay every dial takes. What an operator needs is the moment it changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Escalation {
    /// How many non-open dials in a row there have been.
    pub streak: u32,
    /// The ceiling they crossed into.
    pub cap: Duration,
    /// Whether this process has had a link open at some point. A worker that
    /// never opened is a different pathology from one that has.
    pub has_opened: bool,
}

/// The reconnect ladder's bookkeeping for one process.
#[derive(Debug)]
pub struct ReconnectPolicy {
    health: LinkHealth,
    /// When the current link came up. `None` while there is no link.
    link_started: Option<Instant>,
    /// When a downstream frame last arrived on the current link.
    last_downstream: Option<Instant>,
    /// Whether the current link's counters have already been reset for having
    /// proved itself, so a long-lived link does not keep resetting them.
    counters_reset: bool,
    /// When the staleness decision was last taken, so it runs on its own
    /// cadence rather than on whatever the drain tick happens to be.
    last_stale_check: Option<Instant>,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl ReconnectPolicy {
    pub fn new() -> Self {
        Self {
            health: LinkHealth::new(),
            link_started: None,
            last_downstream: None,
            counters_reset: false,
            last_stale_check: None,
        }
    }

    /// Begin a dial, and return the attempt number to stamp on it.
    ///
    /// Per-link state is cleared here rather than at the end of the previous
    /// link, so a path that returns early cannot leave a stale clock behind and
    /// make the next link look older than it is.
    pub fn begin_dial(&mut self) -> u32 {
        self.link_started = None;
        self.last_downstream = None;
        self.counters_reset = false;
        self.last_stale_check = None;
        self.health.uptime = None;
        self.health.since_last_frame = Duration::ZERO;
        self.health.attempt.max(1)
    }

    /// The socket is up. Not application-ready; the barrier decides that.
    pub fn note_link_opened(&mut self, now: Instant) {
        self.link_started = Some(now);
        self.last_downstream = Some(now);
        self.counters_reset = false;
        self.health.mark_open(Duration::ZERO);
    }

    /// A frame arrived downstream, which is the only thing that proves the far
    /// end is still there.
    pub fn note_downstream(&mut self, now: Instant) {
        self.last_downstream = Some(now);
    }

    /// A dial that never produced an open link.
    pub fn note_dial_failed(&mut self) -> Option<Escalation> {
        self.count_non_open_dial()
    }

    /// A link that opened and then ended.
    ///
    /// A link that stayed up long enough to count as working resets the
    /// counters, because it is the evidence that the worker is not stale. One
    /// that did not is counted as a non-open dial — see this file's header for
    /// why that is the only safe reading.
    pub fn note_link_dropped(&mut self) -> Option<Escalation> {
        self.refresh(Instant::now());
        if self.health.should_reset_counters() {
            self.health.attempt = 1;
            self.health.non_open_streak = 0;
            return None;
        }
        self.count_non_open_dial()
    }

    fn count_non_open_dial(&mut self) -> Option<Escalation> {
        let before = backoff_cap(self.health.non_open_streak, self.health.has_opened);
        self.health.record_non_open();
        let after = backoff_cap(self.health.non_open_streak, self.health.has_opened);
        self.link_started = None;
        self.last_downstream = None;
        if after == AUTH_REJECT_BACKOFF_CAP && before != AUTH_REJECT_BACKOFF_CAP {
            return Some(Escalation {
                streak: self.health.non_open_streak,
                cap: AUTH_REJECT_BACKOFF_CAP,
                has_opened: self.health.has_opened,
            });
        }
        None
    }

    /// Recompute the ages the staleness test reads, from one reading of the
    /// clock.
    ///
    /// One reading for both ages is the point: a link whose uptime and silence
    /// are measured against different moments can satisfy neither test
    /// correctly, and a test that is wrong in the safe direction on one side
    /// and the unsafe direction on the other is worse than no test.
    pub fn refresh(&mut self, now: Instant) {
        self.health.uptime = self
            .link_started
            .map(|started| now.saturating_duration_since(started));
        self.health.since_last_frame = self
            .last_downstream
            .map(|last| now.saturating_duration_since(last))
            .unwrap_or_default();
    }

    /// Whether the staleness decision is due, on its own cadence.
    pub fn stale_check_due(&mut self, now: Instant) -> bool {
        match self.last_stale_check {
            Some(last) if now.saturating_duration_since(last) < STALE_CHECK_INTERVAL => false,
            _ => {
                self.last_stale_check = Some(now);
                self.refresh(now);
                true
            }
        }
    }

    /// Whether the link has gone silent long enough that the far end is gone
    /// even though the socket says otherwise.
    pub fn is_stale(&self) -> bool {
        self.health.is_stale()
    }

    /// How long the link has been silent.
    pub fn silent_for(&self, now: Instant) -> Duration {
        self.last_downstream
            .map(|last| now.saturating_duration_since(last))
            .unwrap_or_default()
    }

    /// Whether the current link has been up long enough to count as working.
    pub fn link_is_stable(&self) -> bool {
        self.link_started
            .is_some_and(|started| started.elapsed() >= STABLE_SESSION)
    }

    /// The delay before the next dial.
    pub fn next_delay(&self) -> Duration {
        self.health.next_delay()
    }

    /// The whole health record, for `roost doctor` and for the diag snapshot.
    pub fn health(&self) -> LinkHealth {
        self.health
    }

    /// The backoff ladder's current attempt number.
    pub fn attempt(&self) -> u32 {
        self.health.attempt
    }
}
