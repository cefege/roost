//! The grid-reading half of predictive local echo: the prediction record, what
//! is painted at a cell, what one frame proves about a prediction, and the pass
//! that applies those verdicts to a burst. `judge_prediction` is the SINGLE
//! place the two asymmetric rules live, and it is pure over one frame and one
//! prediction. Everything here depends only on `roost_protocol::cell` for the
//! column arithmetic and on the parent module's burst state.

use std::collections::BTreeMap;

use roost_protocol::cell::{
    CELL_REVERSE, CellGridFrame, CellSpan, DEFAULT_COLOR, column_span, column_text,
    span_is_atomic,
};

use super::expiry::GLITCH_MS;
use super::{PredictiveEcho, ResetReason};
use crate::store::prefs::PredictMode;

/// The application's own echo latency after the PTY write. A contradiction
/// inside this window is not yet evidence of a wrong guess.
pub const ECHO_GRACE_MS: u64 = 50;

/// One in-flight guess about one cell.
///
/// A prediction is written on every keystroke, including the very first of a
/// burst, and is judged against whatever frames arrive until an authoritative
/// one settles it. The write acknowledgement is the field that decides whether
/// a frame may be held against it at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prediction {
    /// Grid row the guess occupies.
    pub row: u32,
    /// Grid column the guess occupies.
    pub col: u32,
    /// The predicted glyph, or the empty string for an ERASE (backspace over a
    /// plain cell).
    pub ch: String,
    /// Authoritative text at `(row, col)` when the guess was made. A match that
    /// reproduces it proves nothing about our own echo.
    pub original_ch: String,
    /// The confidence epoch this guess belongs to; negative once its epoch has
    /// been dropped.
    pub epoch: i64,
    /// When the keystroke was typed, for RTT sampling and the glitch timer.
    pub born_ms: u64,
    /// Admission sequence of the batch that carried this byte.
    pub input_seq: u64,
    /// When the PTY write was acknowledged; `None` = the worker is not known to
    /// have written this byte.
    pub acked_ms: Option<u64>,
}

/// What one frame proves about one prediction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredictionVerdict {
    /// Retire it AND unlock its epoch: proof our echo landed.
    Credit,
    /// Drop it having proved nothing.
    Retire,
    /// The frame cannot hold the echo yet: no acknowledgement, an ack that has
    /// not landed yet, or a sparse delta that omitted the row. A long-pending
    /// prediction still trips the glitch force-show.
    Unproven,
    /// Contradicted, but inside the application's own echo latency.
    Echoing,
    /// The guess was wrong.
    Contradicted,
}

/// Text painted at viewport `(row, col)` in a frame's run-length spans.
///
/// `None` means a sparse delta did not include the row, while `Some("")` means
/// the represented row is blank or ends before the requested column — the two
/// are different facts and only one of them can hold an echo.
pub fn cell_char_at<'frame>(frame: &'frame CellGridFrame, row: u32, col: i64) -> Option<&'frame str> {
    let found = frame
        .viewport_rows
        .iter()
        .find(|candidate| candidate.index == row)?;
    Some(column_text(&found.spans, col))
}

/// A prediction is satisfied by either representation of an empty cell: a
/// right-trimmed row reads `""`, a row with text to its right reads `" "`.
pub fn prediction_matches(prediction: &Prediction, actual: &str) -> bool {
    if prediction.ch.is_empty() {
        actual.is_empty() || actual == " "
    } else {
        actual == prediction.ch
    }
}

/// What one frame proves about one prediction, judged unconditionally in one
/// place.
///
/// `frame_at_ms` is when the frame ARRIVED: the ack and grace comparisons are
/// meaningless against any other clock. `actual` is `None` when a sparse delta
/// omitted the prediction's row.
pub fn judge_prediction(
    prediction: &Prediction,
    actual: Option<&str>,
    frame_at_ms: u64,
) -> PredictionVerdict {
    if let Some(seen) = actual
        && prediction_matches(prediction, seen)
    {
        // ASYMMETRIC RULE ONE. A GLYPH that replaced DIFFERENT text is proof
        // our echo landed, so it credits even before the write ack: waiting for
        // the ack costs the first chars of every burst a whole extra round trip
        // of invisibility. A glyph reproducing the cell's own prior text proves
        // nothing, and neither does an ERASE — an untouched cell reads blank
        // too, so it would unlock the gate on no evidence at all.
        return if !prediction.ch.is_empty() && prediction.original_ch != prediction.ch {
            PredictionVerdict::Credit
        } else {
            PredictionVerdict::Retire
        };
    }
    // ASYMMETRIC RULE TWO. Judging an unproven prediction as contradicted is
    // what wiped whole bursts mid-typing — "A predicted character flashes the
    // wrong glyph while typing fast", FAILURE-INDEX.md:2390. The frame the
    // predictor sees is the fully folded canonical viewport, so every arriving
    // frame would otherwise judge every in-flight prediction. A prediction may
    // only be CONTRADICTED by grid state that could already hold its echo: the
    // acknowledgement is stamped by client-side input admission, so an unacked
    // prediction is never contradicted, and a contradiction must also outlive
    // the grace. Do not widen what counts as a contradiction to fix a
    // surviving flicker — raise the grace.
    let Some(acked_ms) = prediction.acked_ms else {
        return PredictionVerdict::Unproven;
    };
    if frame_at_ms < acked_ms || actual.is_none() {
        return PredictionVerdict::Unproven;
    }
    if frame_at_ms - acked_ms < ECHO_GRACE_MS {
        PredictionVerdict::Echoing
    } else {
        PredictionVerdict::Contradicted
    }
}

/// What an erase may be painted over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErasableCell {
    /// Nothing is painted there, so no erase is needed at all.
    Blank,
    /// A plain narrow cell on an unstyled background: cover it.
    Erase,
    /// A wide glyph, a grapheme, or a styled cell: do not guess at it.
    Refuse,
}

/// An erase cell paints the default terminal background over one column, so it
/// may only cover a plain narrow cell on an unstyled background.
pub fn erasable_cell(spans: &[CellSpan], col: i64) -> ErasableCell {
    let Some((span, _offset)) = column_span(spans, col) else {
        return ErasableCell::Blank;
    };
    if span_is_atomic(span) {
        return ErasableCell::Refuse;
    }
    if span.bg != DEFAULT_COLOR || span.bg_rgb.is_some() {
        return ErasableCell::Refuse;
    }
    if span.flags & CELL_REVERSE != 0 {
        return ErasableCell::Refuse;
    }
    ErasableCell::Erase
}

impl PredictiveEcho {
    /// One printable width-1 glyph at the predicted caret column.
    ///
    /// The cell's own prior text is recorded with the guess, because a frame
    /// that merely reproduces it is not evidence this keystroke echoed.
    pub(super) fn predict_glyph(&mut self, byte: u8, input_seq: u64, now_ms: u64) {
        let col = self.predicted_cursor_col.unwrap_or(self.cursor_col);
        if col + 1 >= self.cols {
            // A last-column wrap is ambiguous about where the glyph lands.
            self.become_tentative();
            return;
        }
        // A glyph typed over our own eraser supersedes it; keeping both would
        // make the eraser contradict the very echo that confirms the glyph.
        if let Some(stale) = self
            .preds
            .iter()
            .position(|pred| pred.ch.is_empty() && pred.row == self.cursor_row && pred.col == col)
        {
            self.preds.remove(stale);
        }
        let original_ch = column_text(self.cursor_spans(), i64::from(col)).to_string();
        self.preds.push(Prediction {
            row: self.cursor_row,
            col,
            ch: String::from(char::from(byte)),
            original_ch,
            epoch: self.prediction_epoch,
            born_ms: now_ms,
            input_seq,
            acked_ms: None,
        });
        self.predicted_cursor_col = Some(col + 1);
    }

    /// A left/right arrow (CSI 'C'/'D') predicts the cursor move only, never a
    /// glyph, so the caret leads with nothing painted over the grid.
    pub(super) fn predict_arrow(&mut self, right: bool) {
        let base = self.predicted_cursor_col.unwrap_or(self.cursor_col);
        let moved = if right {
            base.saturating_add(1)
        } else {
            base.saturating_sub(1)
        };
        self.predicted_cursor_col = Some(moved.min(self.cols.saturating_sub(1)));
    }

    /// Backspace: paint an ERASE over the column the echo is about to clear.
    /// Moving the caret alone leaves the authoritative glyph under it for a
    /// full round trip, which reads as "my correction did nothing".
    pub(super) fn predict_erase(&mut self, input_seq: u64, now_ms: u64) {
        let anchor = self.predicted_cursor_col.unwrap_or(self.cursor_col);
        let Some(col) = anchor.checked_sub(1) else {
            self.become_tentative();
            return;
        };
        self.predicted_cursor_col = Some(col);
        if let Some(owned) = self
            .preds
            .iter()
            .position(|pred| pred.row == self.cursor_row && pred.col == col)
        {
            // Erasing our own guess: drop it, and paint nothing over it.
            self.preds.remove(owned);
            return;
        }
        let erasable = erasable_cell(self.cursor_spans(), i64::from(col));
        match erasable {
            ErasableCell::Blank => return,
            ErasableCell::Refuse => {
                self.become_tentative();
                return;
            }
            ErasableCell::Erase => {}
        }
        let original_ch = column_text(self.cursor_spans(), i64::from(col)).to_string();
        self.preds.push(Prediction {
            row: self.cursor_row,
            col,
            ch: String::new(),
            original_ch,
            epoch: self.prediction_epoch,
            born_ms: now_ms,
            input_seq,
            acked_ms: None,
        });
    }

    /// Judge every prediction against one frame. `frame_at_ms` is when the frame
    /// ARRIVED, so a deferred pass can never inflate the sampled RTT and the
    /// ack and grace comparisons stay meaningful.
    pub(super) fn reconcile_against(&mut self, frame: &CellGridFrame, frame_at_ms: u64) {
        let mut preds = std::mem::take(&mut self.preds);
        // The FIRST tentative of an epoch is the one that unlocks it; the rest
        // stay hidden until a later frame, so one echo never reveals the whole
        // tail of a burst.
        let mut first_tentative: BTreeMap<i64, usize> = BTreeMap::new();
        for (index, pred) in preds.iter().enumerate() {
            if self.is_tentative(pred) {
                first_tentative.entry(pred.epoch).or_insert(index);
            }
        }
        let mut survivors: Vec<Prediction> = Vec::with_capacity(preds.len());
        let mut hard_reset = false;
        // A cursor, not an iterator: dropping a hidden epoch RETAGS the rest of
        // it, and the very next prediction of that epoch must be judged as the
        // now-shown guess it became. An `iter()` over `preds` could not be
        // mutated from inside its own loop.
        let mut index = 0;
        while index < preds.len() {
            let pred = preds[index].clone();
            let shown_before = !self.is_tentative(&pred);
            let actual = cell_char_at(frame, pred.row, i64::from(pred.col));
            match judge_prediction(&pred, actual, frame_at_ms) {
                PredictionVerdict::Credit => {
                    // The first tentative of an epoch unlocks it; the rest stay
                    // hidden until a later frame.
                    if self.is_tentative(&pred) && first_tentative.get(&pred.epoch) != Some(&index) {
                        survivors.push(pred);
                    } else {
                        self.confirmed_epoch = self.confirmed_epoch.max(pred.epoch);
                        self.sample_rtt(frame_at_ms.saturating_sub(pred.born_ms));
                        self.glitch = false;
                    }
                }
                PredictionVerdict::Retire => self.glitch = false,
                PredictionVerdict::Unproven => {
                    if frame_at_ms.saturating_sub(pred.born_ms) >= GLITCH_MS {
                        self.glitch = true;
                    }
                    survivors.push(pred);
                }
                PredictionVerdict::Echoing => survivors.push(pred),
                PredictionVerdict::Contradicted => {
                    // Experimental mode drops just the wrong cell — never a hard
                    // reset, never an epoch kill. Flickerier, but each cell
                    // self-corrects independently.
                    if self.mode == PredictMode::Experimental {
                        index += 1;
                        continue;
                    }
                    if shown_before {
                        hard_reset = true;
                        break;
                    }
                    // A hidden wrong guess drops just its epoch, and re-arms the
                    // confidence gate.
                    for other in preds.iter_mut() {
                        if other.epoch == pred.epoch {
                            other.epoch = -1;
                        }
                    }
                    self.become_tentative();
                }
            }
            index += 1;
        }
        if hard_reset {
            self.reset_all(ResetReason::Contradicted);
            return;
        }
        self.preds = survivors
            .into_iter()
            .filter(|pred| pred.epoch >= 0)
            .collect();
        self.reanchor_predicted_cursor();
        self.arm_expiry(frame_at_ms);
    }

    /// The final surviving prediction already carries the absolute predicted
    /// caret. Deriving it from the authoritative cursor plus a survivor count
    /// double-counts echoes when a sparse cursor-only frame omits the row that
    /// changed.
    fn reanchor_predicted_cursor(&mut self) {
        self.predicted_cursor_col = self.preds.last().and_then(|last| {
            let predicted = i64::from(last.col) + i64::from(!last.ch.is_empty());
            (predicted >= 0 && predicted < i64::from(self.cols)).then_some(predicted as u32)
        });
    }
}
