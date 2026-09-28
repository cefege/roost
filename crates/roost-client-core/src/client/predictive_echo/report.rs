//! The shapes the predictor REPORTS back to its caller: the cells the overlay
//! paints, the paint request itself, the state a diagnostic reads, and the
//! reason a burst was abandoned. Every field is a decision the state machine in
//! `super` actually made, so a host can assert on a report without a DOM —
//! which is what the overlay geometry in `roost-web-terminal` and this crate's
//! own tests both do. Nothing here decides anything; `super` owns every rule.
//! Ports the `PredictedCell` / `_debug()` shapes of v2's
//! `apps/web/src/renderer/predictiveEchoOverlay.ts` and `predictiveEcho.ts`.

use crate::store::prefs::PredictMode;

/// Why a burst was abandoned. `Cleared` is the only reason an EXTERNAL wipe
/// produces, which is a different defect from the engine's own rules firing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetReason {
    /// The pane's stall watchdog asked for it.
    Cleared,
    /// No frame settled the oldest prediction inside the expiry window.
    Expired,
    /// A prediction that had been SHOWN was contradicted.
    Contradicted,
    /// Alt-screen was entered or left, so every prediction coordinate is void.
    AltScreen,
    /// The grid's column or row count changed under the burst.
    Resized,
    /// History was appended, which shifts the rows a guess addressed.
    Scrolled,
    /// Prediction was switched off, or typed into the alternate screen.
    Suppressed,
    Paste,
    Preference,
}

impl ResetReason {
    /// The stable name this reason is logged and reported under.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cleared => "cleared",
            Self::Expired => "expired",
            Self::Contradicted => "contradicted",
            Self::AltScreen => "alt_screen",
            Self::Resized => "resized",
            Self::Scrolled => "scrolled",
            Self::Suppressed => "suppressed",
            Self::Paste => "paste",
            Self::Preference => "preference",
        }
    }
}

/// One predicted cell the overlay paints over the grid, addressed by the grid
/// `(row, col)` it covers. `ch` is the predicted glyph, or the empty string for
/// an ERASE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictedCell {
    pub row: u32,
    pub col: u32,
    pub ch: String,
}

/// What the overlay should paint, or `None` to clear it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoPaint {
    /// The visible cells; hidden tentative ones are already removed.
    pub cells: Vec<PredictedCell>,
    /// Underline the GLYPH cells: the guess is shown because the link is slow.
    pub flagged: bool,
    /// The column the predicted caret is painted at, or `None` while any
    /// prediction in the burst is still hidden: a caret must never lead text
    /// the user cannot see.
    pub caret_col: Option<u32>,
}

/// The burst's internal state, for diagnostics and tests. Every field is a
/// decision the machine actually made, so no DOM assertion is needed.
#[derive(Debug, Clone, PartialEq)]
pub struct EchoDebug {
    /// Predictions still in flight, and how many the overlay would paint.
    pub total: usize,
    pub visible: usize,
    /// EWMA of echo RTT; zero until the first sample.
    pub srtt_ms: f64,
    /// The highest epoch an echo has proven, and the epoch new guesses join; a
    /// prediction above the confirmed one is hidden.
    pub confirmed_epoch: i64,
    pub prediction_epoch: i64,
    /// The display mode in force.
    pub mode: PredictMode,
    /// The predicted caret column, or `None` when it does not lead.
    pub predicted_cursor_col: Option<u32>,
    /// Every abandon, and only the external wipes — which is what a zero
    /// `reset_count` delta across a sustained burst is really about.
    pub reset_count: u32,
    pub cleared_count: u32,
    /// The last abandon's reason.
    pub last_reset: Option<ResetReason>,
}
