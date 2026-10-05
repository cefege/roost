//! The quiet probe's anchor and its backoff: how long a foreground pane may stay
//! silent before it is challenged. Owned by `liveness::ForegroundLiveness`;
//! fed by `session_liveness` on every accepted frame.
//!
//! A pane that answers a challenge with nothing but the proof is idle, not
//! stuck. Challenging it at the base interval forever re-baselines a healthy
//! pane every five seconds, so each proof with no output between doubles the
//! next interval, up to `TERMINAL_FOREGROUND_IDLE_PROBE_MAX_MS`; any real output
//! drops it back to the base.

use roost_protocol::viewport::{
    TERMINAL_FOREGROUND_IDLE_PROBE_MAX_MS, TERMINAL_FOREGROUND_IDLE_PROBE_MS,
};

/// The doublings past which the interval is capped anyway; it keeps the shift
/// in range for any streak.
const MAX_DOUBLINGS: u32 = 8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdleProbe {
    /// Consecutive challenges answered with no other frame between them.
    proved_idle_streak: u32,
    /// The instant the armed probe was anchored at.
    anchor_ms: Option<u64>,
}

impl IdleProbe {
    /// Challenges answered in a row with no output between them.
    pub fn proved_idle_streak(&self) -> u32 {
        self.proved_idle_streak
    }

    /// The silence the next probe allows: the base interval doubled once per
    /// idle proof, capped.
    pub fn backed_off_interval_ms(&self) -> u64 {
        TERMINAL_FOREGROUND_IDLE_PROBE_MS
            .saturating_mul(1 << self.proved_idle_streak.min(MAX_DOUBLINGS))
            .min(TERMINAL_FOREGROUND_IDLE_PROBE_MAX_MS)
    }

    /// Anchor a probe at `anchor_ms`, `interval_ms` long; returns when it is due.
    pub fn anchor(&mut self, anchor_ms: u64, interval_ms: u64) -> u64 {
        self.anchor_ms = Some(anchor_ms);
        anchor_ms.saturating_add(interval_ms)
    }

    /// The anchor of the probe being taken, cleared.
    pub fn take_anchor(&mut self) -> Option<u64> {
        self.anchor_ms.take()
    }

    /// A challenge was answered by its proof alone.
    pub fn note_proved(&mut self) {
        self.proved_idle_streak = self.proved_idle_streak.saturating_add(1);
    }

    /// A frame that was not a proof arrived: the pane is producing output.
    pub fn note_output(&mut self) {
        self.proved_idle_streak = 0;
    }

    /// Forget the anchor and the streak, for a retired replica.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
