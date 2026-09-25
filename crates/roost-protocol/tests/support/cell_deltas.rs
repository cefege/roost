//! Hand-built cell-grid fixtures for the sparse delta-batch tests.
//!
//! The rows are literal `CellRow` values rather than an encoder's output, so
//! the fold is exercised without a terminal core and every rule can be
//! violated in isolation.

use std::sync::Arc;

use roost_protocol::cell::delta_batch::CellDeltaBatch;
use roost_protocol::cell::types::{CellGridFrame, CellRow, CellSpan, DEFAULT_COLOR, MouseTracking};

const STREAM_ID: &str = "00000000-0000-4000-8000-000000000001";

pub fn text_row(index: u32, text: &str) -> CellRow {
    if text.is_empty() {
        return CellRow {
            index,
            spans: Arc::from(Vec::new()),
        };
    }
    CellRow {
        index,
        spans: Arc::from(vec![CellSpan {
            text: text.to_owned(),
            fg: DEFAULT_COLOR,
            bg: DEFAULT_COLOR,
            flags: 0,
            fg_rgb: None,
            bg_rgb: None,
            columns: text.chars().count() as u32,
            link_uri: None,
            link_key: None,
        }]),
    }
}

pub fn full_frame(rows: &[&str]) -> CellGridFrame {
    CellGridFrame {
        stream_id: STREAM_ID.to_owned(),
        grid_epoch: "batch-grid:0".to_owned(),
        cols: 20,
        rows: rows.len() as u32,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: MouseTracking::None,
        mouse_sgr: false,
        focus_events: false,
        full: true,
        viewport_rows: rows
            .iter()
            .enumerate()
            .map(|(at, text)| text_row(at as u32, text))
            .collect(),
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq: 0,
        seq: 1,
    }
}

pub fn next_delta(
    base: &CellGridFrame,
    viewport_rows: Vec<CellRow>,
    appended: Vec<CellRow>,
) -> CellGridFrame {
    let scrollback_append_len = appended.len() as u64;
    CellGridFrame {
        full: false,
        viewport_rows,
        scrollback_rows: Vec::new(),
        scrollback_append: appended,
        scrollback_total: base.scrollback_total + scrollback_append_len,
        sb_base: 0,
        base_seq: base.seq,
        seq: base.seq + 1,
        ..base.clone()
    }
}

pub fn row_text(rows: &[CellRow]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.text.clone())
                .collect::<String>()
        })
        .collect()
}

pub fn indices(rows: &[CellRow]) -> Vec<u32> {
    rows.iter().map(|row| row.index).collect()
}
