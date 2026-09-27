//! Predictive (speculative) local echo: the burst state machine over Roost's
//! cell stream. It paints a typed character immediately instead of waiting a
//! full round trip, then reconciles the guess against the authoritative frame.
//! Driven by the terminal pane: `predict` per keystroke, `note_input_written`
//! per input admission, `on_frame` per frame. `grid` owns every judgement and
//! `expiry` the timing, and every clock read is a `now_ms` argument.

pub mod expiry;
pub mod grid;

use std::sync::Arc;

use roost_protocol::cell::{CellGridFrame, CellSpan};

use crate::client::predictive_echo::expiry::{ExpiryCheck, expiry_check, expiry_delay_ms};
use crate::client::predictive_echo::grid::Prediction;
use crate::store::prefs::PredictMode;

/// srtt/2 above this engages predictions. Low enough that local echo engages on
/// a LAN or tailnet link (~15-30 ms RTT); only a single-digit-ms loopback is a
/// no-op.
pub const SHOW_ON_MS: f64 = 5.0;
/// srtt/2 at or below this disarms, and only while nothing is in flight.
pub const SHOW_OFF_MS: f64 = 3.0;
/// The longest input batch still treated as a keystroke. A paste floods the
/// overlay and its echo is unguessable, so it resets the burst instead.
pub const MAX_PREDICTED_INPUT_BYTES: usize = 100;

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

/// The prediction state machine for one pane's viewport.
#[derive(Debug)]
pub struct PredictiveEcho {
    preds: Vec<Prediction>,
    prediction_epoch: i64,
    confirmed_epoch: i64,
    /// EWMA of echo RTT in milliseconds; zero = unmeasured.
    srtt_ms: f64,
    /// A prediction pending this long is force-shown: the link has stalled
    /// rather than merely being slow, and waiting longer helps nobody.
    glitch: bool,
    /// The display gate's hysteresis latch; `expiry` owns the rule.
    srtt_trigger: bool,
    reset_count: u32,
    cleared_count: u32,
    last_reset: Option<ResetReason>,

    // Authoritative grid state, updated each frame.
    cursor_row: u32,
    cursor_col: u32,
    cols: u32,
    /// The last-seen viewport HEIGHT, which detects a resize; a delta's row
    /// count is its DIRTY-ROW count and drifts every frame.
    rows: u32,
    alt_screen: bool,
    cursor_row_spans: Option<Arc<[CellSpan]>>,
    predicted_cursor_col: Option<u32>,
    /// The delay a host should schedule to abandon the oldest prediction.
    expiry_delay_ms: Option<u64>,
    mode: PredictMode,
}

impl PredictiveEcho {
    /// An anchored, empty burst in `mode`.
    pub const fn new(mode: PredictMode) -> Self {
        Self {
            preds: Vec::new(),
            prediction_epoch: 1,
            confirmed_epoch: 0,
            srtt_ms: 0.0,
            glitch: false,
            srtt_trigger: false,
            reset_count: 0,
            cleared_count: 0,
            last_reset: None,
            cursor_row: 0,
            cursor_col: 0,
            cols: 0,
            rows: 0,
            alt_screen: false,
            cursor_row_spans: None,
            predicted_cursor_col: None,
            expiry_delay_ms: None,
            mode,
        }
    }

    /// The display mode in force.
    pub const fn mode(&self) -> PredictMode {
        self.mode
    }

    /// The delay a host should schedule to abandon the oldest prediction.
    pub const fn expiry_delay_ms(&self) -> Option<u64> {
        self.expiry_delay_ms
    }

    /// Apply a Settings change at once, even while the terminal is idle.
    pub fn set_mode(&mut self, mode: PredictMode) {
        self.mode = mode;
        if mode == PredictMode::Never {
            self.reset_all(ResetReason::Preference);
        }
    }

    /// Abandon every prediction. Called by the pane's stall watchdog, which is
    /// an EXTERNAL wipe of guesses that were correct.
    pub fn clear(&mut self) {
        self.reset_all(ResetReason::Cleared);
    }

    /// Drop the burst and stop asking for an expiry pass.
    pub fn dispose(&mut self) {
        self.expiry_delay_ms = None;
        self.preds.clear();
        self.predicted_cursor_col = None;
    }

    /// A keystroke the user typed. Predict its echo — always, to measure RTT;
    /// display is gated in `paint_request`. `input_seq` is the admission
    /// sequence of the batch carrying these bytes; `note_input_written` later
    /// proves the worker wrote them, and until it does no frame can contradict
    /// the guess.
    pub fn predict(&mut self, bytes: &[u8], input_seq: u64, now_ms: u64) {
        if self.mode == PredictMode::Never || self.alt_screen {
            if !self.preds.is_empty() {
                self.reset_all(ResetReason::Suppressed);
            }
            return;
        }
        if bytes.len() > MAX_PREDICTED_INPUT_BYTES {
            self.reset_all(ResetReason::Paste);
            return;
        }
        // Experimental mode has no tentative epoch: predictions show
        // IMMEDIATELY, trading the no-flicker guarantee for zero-latency display.
        if self.mode == PredictMode::Experimental {
            self.prediction_epoch = self.confirmed_epoch;
        }
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            let arrow = byte == 0x1b
                && matches!(bytes.get(index + 1..index + 3), Some([0x5b, 0x43 | 0x44]));
            if arrow {
                self.predict_arrow(bytes[index + 2] == 0x43);
            } else if byte == 0x7f || byte == 0x08 {
                self.predict_erase(input_seq, now_ms);
            } else if !(0x20..=0x7e).contains(&byte) {
                // Refuse: control, DEL handled above, ESC/CSI start, and high
                // bytes (multi-byte UTF-8 / wide) — all ambiguous.
                self.become_tentative();
            } else {
                self.predict_glyph(byte, input_seq, now_ms);
            }
            // An arrow's introducer and final byte belong to the sequence, not
            // to the keystroke stream, and must not become predicted glyphs.
            index += if arrow { 3 } else { 1 };
        }
        self.arm_expiry(now_ms);
    }

    /// The worker acknowledged writing every byte up to `input_seq` to the PTY.
    /// Predictions from those batches become judgeable after `ECHO_GRACE_MS`.
    pub fn note_input_written(&mut self, input_seq: u64, now_ms: u64) {
        for pred in self.preds.iter_mut() {
            if pred.acked_ms.is_none() && pred.input_seq <= input_seq {
                pred.acked_ms = Some(now_ms);
            }
        }
        self.arm_expiry(now_ms);
    }

    /// An authoritative cell frame landed: update grid state, reconcile every
    /// prediction against it, then leave the survivors to be repainted.
    ///
    /// `scrollback_appended` is the caller's batch-history signal, NOT a field
    /// of the frame: a coalesced batch knows it carried history the frame it
    /// finally hands over no longer lists.
    pub fn on_frame(&mut self, frame: &CellGridFrame, now_ms: u64, scrollback_appended: bool) {
        let previous_alt = self.alt_screen;
        let previous_cols = self.cols;
        let previous_rows = self.rows;
        self.cursor_row = frame.cursor_row;
        self.cursor_col = frame.cursor_col;
        self.cols = frame.cols;
        self.alt_screen = frame.alt_screen;
        self.rows = frame.rows;
        self.cursor_row_spans = frame
            .viewport_rows
            .iter()
            .find(|row| row.index == frame.cursor_row)
            .map(|row| Arc::clone(&row.spans));

        // Wipe ONLY when prediction coordinates are actually invalidated. A
        // non-resize full or delta frame keeps the same viewport coords, so
        // RECONCILE against it — wiping on every full frame kills predictions
        // before the echo delta can confirm them, and SRTT is never sampled.
        let resized =
            previous_cols != 0 && (frame.cols != previous_cols || frame.rows != previous_rows);
        if self.alt_screen || previous_alt != self.alt_screen || scrollback_appended || resized {
            let reason = if self.alt_screen || previous_alt != self.alt_screen {
                ResetReason::AltScreen
            } else if resized {
                ResetReason::Resized
            } else {
                ResetReason::Scrolled
            };
            self.reset_all(reason);
            return;
        }
        self.reconcile_against(frame, now_ms);
    }

    /// The deferred expiry pass, driven against an injected clock rather than
    /// a real timer.
    pub fn expire_predictions(&mut self, now_ms: u64) {
        if expiry_check(self.oldest_born_ms(), now_ms, self.srtt_ms) == ExpiryCheck::Expired {
            self.reset_all(ResetReason::Expired);
        }
        self.arm_expiry(now_ms);
    }

    /// What the overlay should paint right now, or `None` to clear it.
    pub fn paint_request(&mut self) -> Option<EchoPaint> {
        if self.mode == PredictMode::Never || self.alt_screen {
            return None;
        }
        self.arm_display();
        if !self.should_show() {
            return None;
        }
        // The caret leads echoed characters or an arrow move, never text that
        // is still tentative and therefore hidden.
        let blocked = self.preds.iter().any(|pred| self.is_tentative(pred));
        let cells = self
            .preds
            .iter()
            .filter(|pred| !self.is_tentative(pred))
            .map(|pred| PredictedCell {
                row: pred.row,
                col: pred.col,
                ch: pred.ch.clone(),
            })
            .collect();
        Some(EchoPaint {
            cells,
            flagged: self.should_flag(),
            caret_col: if blocked {
                None
            } else {
                self.predicted_cursor_col
            },
        })
    }

    /// The burst's internal state, for diagnostics and tests.
    pub fn debug(&mut self) -> EchoDebug {
        // The gate is only consulted, and so only armed, when the overlay would
        // actually paint — asking it in `never` or alt-screen never happens.
        let showing = self.mode != PredictMode::Never && !self.alt_screen && {
            self.arm_display();
            self.should_show()
        };
        let hidden = self
            .preds
            .iter()
            .filter(|pred| self.is_tentative(pred))
            .count();
        EchoDebug {
            total: self.preds.len(),
            visible: if showing {
                self.preds.len() - hidden
            } else {
                0
            },
            srtt_ms: self.srtt_ms,
            confirmed_epoch: self.confirmed_epoch,
            prediction_epoch: self.prediction_epoch,
            mode: self.mode,
            predicted_cursor_col: self.predicted_cursor_col,
            reset_count: self.reset_count,
            cleared_count: self.cleared_count,
            last_reset: self.last_reset,
        }
    }

    /// Drop every prediction AND re-arm the confidence gate. Without the
    /// re-arm, the next keystroke is shown on an authoritative cursor column
    /// that still lags the un-echoed input — the wrong glyph the user sees snap
    /// back (FAILURE-INDEX.md:2390).
    fn reset_all(&mut self, reason: ResetReason) {
        self.preds.clear();
        self.predicted_cursor_col = None;
        self.glitch = false;
        self.srtt_trigger = false;
        self.reset_count += 1;
        self.cleared_count += u32::from(reason == ResetReason::Cleared);
        self.last_reset = Some(reason);
        self.become_tentative();
        self.expiry_delay_ms = None;
        tracing::debug!(target: "echo", reason = reason.as_str(), "predictive echo reset");
    }

    fn become_tentative(&mut self) {
        self.prediction_epoch += 1;
    }

    /// A prediction is hidden while its epoch is unproven.
    fn is_tentative(&self, pred: &Prediction) -> bool {
        pred.epoch > self.confirmed_epoch
    }

    /// The cursor row's spans, or nothing before the first frame carrying it.
    fn cursor_spans(&self) -> &[CellSpan] {
        self.cursor_row_spans.as_deref().unwrap_or(&[])
    }

    fn oldest_born_ms(&self) -> Option<u64> {
        self.preds.iter().map(|pred| pred.born_ms).min()
    }

    fn arm_expiry(&mut self, now_ms: u64) {
        self.expiry_delay_ms = expiry_delay_ms(self.oldest_born_ms(), now_ms, self.srtt_ms);
    }
}
