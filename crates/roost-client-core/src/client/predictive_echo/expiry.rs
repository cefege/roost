//! The timing half of predictive local echo: the echo RTT estimate, the display
//! gate that estimate arms, and the clock that abandons a guess no
//! authoritative frame ever settled. A non-echoing prompt (sudo, an ssh
//! password) produces no later frame at all, so without that clock a guess
//! stays painted forever. Every time here is a `now_ms` argument, so a test
//! drives the same numbers a host does. Ports v2's
//! `apps/web/src/client/input/predictiveEchoExpiry.ts` plus the RTT and gate
//! rules of `apps/web/src/renderer/predictiveEcho.ts`.

use super::{PredictiveEcho, SHOW_OFF_MS, SHOW_ON_MS};
use crate::store::prefs::PredictMode;

/// The shortest window a prediction gets, when the echo RTT is unmeasured or
/// fast. It is deliberately longer than any plausible echo so a legitimate
/// prediction is never expired out from under it.
pub const PREDICTION_EXPIRE_FLOOR_MS: u64 = 1_000;

/// srtt/2 above this underlines the guess, so a shown prediction reads as one.
pub const FLAG_ON_MS: f64 = 80.0;

/// A prediction pending this long is force-shown: the link is stalled, not slow.
pub const GLITCH_MS: u64 = 250;

/// The longest sample an echo round trip may contribute. A backgrounded tab
/// wakes up to a multi-second gap that says nothing about the link.
const MAX_RTT_SAMPLE_MS: f64 = 5_000.0;

/// The EWMA weight a new sample takes; the rest is history.
const RTT_SAMPLE_WEIGHT: f64 = 0.125;

/// What a fired expiry pass found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryCheck {
    /// The oldest live prediction outlived the window: abandon every prediction.
    Expired,
    /// Nothing is live, so there is nothing to arm.
    Idle,
    /// Not yet expired — re-arm for this many milliseconds.
    Rearm {
        /// Milliseconds until the oldest live prediction outlives the window.
        delay_ms: u64,
    },
}

/// The window a prediction must survive to be abandoned.
///
/// A measured slow link widens it, so a legitimate prediction on a genuinely
/// laggy connection is never expired out from under its echo. A zero estimate
/// is an unmeasured link and takes the floor.
pub fn expiry_window_ms(srtt_ms: f64) -> u64 {
    let widened = (srtt_ms.max(0.0) * 4.0) as u64;
    widened.max(PREDICTION_EXPIRE_FLOOR_MS)
}

/// The delay a host should schedule to abandon the oldest live prediction, or
/// `None` when there are none.
pub fn expiry_delay_ms(oldest_born_ms: Option<u64>, now_ms: u64, srtt_ms: f64) -> Option<u64> {
    let oldest = oldest_born_ms?;
    Some(remaining_ms(oldest, now_ms, srtt_ms))
}

/// Expire when the oldest live prediction outlived the window, else re-arm.
pub fn expiry_check(oldest_born_ms: Option<u64>, now_ms: u64, srtt_ms: f64) -> ExpiryCheck {
    let Some(oldest) = oldest_born_ms else {
        return ExpiryCheck::Idle;
    };
    if now_ms.saturating_sub(oldest) >= expiry_window_ms(srtt_ms) {
        ExpiryCheck::Expired
    } else {
        ExpiryCheck::Rearm {
            delay_ms: remaining_ms(oldest, now_ms, srtt_ms),
        }
    }
}

/// Never zero: a pass scheduled for the current millisecond is a spin, and the
/// window only ever matters as an upper bound on how long a guess survives.
fn remaining_ms(oldest_born_ms: u64, now_ms: u64, srtt_ms: f64) -> u64 {
    oldest_born_ms
        .saturating_add(expiry_window_ms(srtt_ms))
        .saturating_sub(now_ms)
        .max(1)
}

impl PredictiveEcho {
    /// Fold one round-trip sample into the EWMA.
    ///
    /// A zero sample is a frame that arrived in the same millisecond the
    /// keystroke was born: that measures the clock's resolution, not the link,
    /// and installing it would pin the display gate on forever.
    ///
    /// The FIRST sample seeds the estimate outright rather than being weighted
    /// against a zero that means "unmeasured", so the very first keystroke of a
    /// session can already arm the gate.
    pub(super) fn sample_rtt(&mut self, rtt_ms: u64) {
        let sample = rtt_ms as f64;
        if sample <= 0.0 || sample > MAX_RTT_SAMPLE_MS {
            return;
        }
        self.srtt_ms = if self.srtt_ms == 0.0 {
            sample
        } else {
            self.srtt_ms * (1.0 - RTT_SAMPLE_WEIGHT) + sample * RTT_SAMPLE_WEIGHT
        };
        tracing::debug!(target: "echo", rtt_ms, "predictive echo rtt sample");
    }

    /// Arm or disarm the display gate.
    ///
    /// It is deliberately STATEFUL: a prediction arriving inside the 3-5 ms
    /// dead-band must not silently turn the display off, so the trigger is
    /// sticky and clears only when the link is fast AND nothing is in flight. A
    /// stateless recomputation returns false inside that band, so nothing would
    /// ever paint there.
    pub(super) fn arm_display(&mut self) {
        if self.mode == PredictMode::Always || self.mode == PredictMode::Experimental {
            return;
        }
        let half = self.srtt_ms / 2.0;
        if self.glitch || half > SHOW_ON_MS {
            self.srtt_trigger = true;
        } else if half <= SHOW_OFF_MS && self.preds.is_empty() {
            self.srtt_trigger = false;
        }
    }

    /// Whether the display gate lets a prediction be painted at all. `Always`
    /// and `Experimental` bypass it entirely; `Never` is refused by the caller
    /// before this is asked.
    pub(super) fn should_show(&self) -> bool {
        match self.mode {
            PredictMode::Always | PredictMode::Experimental => true,
            PredictMode::Never | PredictMode::Adaptive => self.srtt_trigger,
        }
    }

    /// Whether the painted cells are underlined, which says "this is a guess on
    /// a slow link" rather than "this is what the terminal drew".
    pub(super) fn should_flag(&self) -> bool {
        self.glitch || self.srtt_ms / 2.0 > FLAG_ON_MS
    }
}
