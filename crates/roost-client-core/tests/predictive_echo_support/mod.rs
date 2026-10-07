//! The fixtures the predictive-echo test binaries share: the display-gate
//! cases, the confidence-gate cases and the burst-abandoning cases all build
//! frames and drive a predictor the same way, so the construction lives once
//! here rather than three times with the three copies drifting.
//!
//! Depends only on the engine itself: `PredictiveEcho`, its report types, the
//! grid-frame shapes, and the predictor mode enum.

#![allow(dead_code)]

use std::sync::Arc;

use roost_client_core::client::predictive_echo::PredictiveEcho;
use roost_client_core::store::prefs::PredictMode;
use roost_protocol::cell::{CellGridFrame, CellRow, CellSpan, MouseTracking};

/// One default-styled run of `text`, one column per scalar.
pub fn plain_span(text: &str) -> CellSpan {
    CellSpan {
        text: text.to_string(),
        fg: 256,
        bg: 256,
        flags: 0,
        fg_rgb: None,
        bg_rgb: None,
        columns: text.chars().count() as u32,
        link_uri: None,
        link_key: None,
    }
}

/// A frame carrying `text` on one row, with the cursor where it is told.
pub fn frame(seq: u64, cursor_row: u32, cursor_col: u32, row: u32, text: &str) -> CellGridFrame {
    empty_frame(
        seq,
        cursor_row,
        cursor_col,
        vec![(row, Some(vec![plain_span(text)]))],
    )
}

/// A frame with no rows at all, which is what a coalesced empty batch carries.
pub fn empty_frame(
    seq: u64,
    cursor_row: u32,
    cursor_col: u32,
    rows: Vec<(u32, Option<Vec<CellSpan>>)>,
) -> CellGridFrame {
    CellGridFrame {
        stream_id: "echo-test:0".to_string(),
        grid_epoch: "echo-grid:0".to_string(),
        cols: 80,
        rows: 24,
        cursor_row,
        cursor_col,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
        full: false,
        mouse_tracking: MouseTracking::None,
        mouse_sgr: false,
        focus_events: false,
        kitty_keyboard_flags: 0,
        viewport_rows: rows
            .into_iter()
            .map(|(index, spans)| CellRow {
                index,
                mark: 0,
                spans: Arc::from(spans.unwrap_or_default()),
            })
            .collect(),
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq: seq.saturating_sub(1),
        seq,
    }
}

/// An anchored predictor in `mode`, at t=0 with the cursor at column 0.
pub fn anchored(mode: PredictMode) -> PredictiveEcho {
    let mut echo = PredictiveEcho::new(mode);
    echo.on_frame(&empty_frame(1, 0, 0, Vec::new()), 0, false);
    echo
}

/// Type a keystroke AND acknowledge its PTY write.
pub fn typed(echo: &mut PredictiveEcho, bytes: &[u8], input_seq: u64, now_ms: u64) {
    echo.predict(bytes, input_seq, now_ms);
    echo.note_input_written(input_seq, now_ms);
}
