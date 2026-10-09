//! The two published presentations: what a paint shows, and the snapshot
//! diagnostics diff a stalled pane against.
//!
//! The window the paint presentation chooses and the values the snapshot
//! reports are the numbers a reader of a frozen pane reasons from, so they are
//! pinned here rather than inferred from a screenshot: a window that does not
//! contain the reader's own row is a window that reports the wrong part of
//! history, and a snapshot that says `at_bottom` for a pane that is not there
//! sends the diagnosis the wrong way.

use std::rc::Rc;
use std::sync::Arc;

use roost_protocol::cell::{CellGridFrame, CellRow, CellSpan, MouseTracking};
use roost_web_terminal::presentation::{
    RendererEpochSeq, RendererPresentationSnapshot, RendererProjection,
    RendererTerminalModeSnapshot, create_renderer_paint_presentation,
    create_renderer_presentation_snapshot, same_scrollback_row,
};
use roost_web_terminal::reader_intent::{ReaderAnchor, ReaderIntent, ReaderIntentReason};

fn row(index: u32) -> CellRow {
    CellRow {
        index,
        mark: 0,
        spans: Arc::from([CellSpan {
            text: format!("row {index}"),
            fg: 7,
            bg: 256,
            flags: 0,
            fg_rgb: None,
            bg_rgb: None,
            columns: 8,
            link_uri: None,
            link_key: None,
        }]),
    }
}

fn painted(indices: std::ops::Range<u32>) -> Vec<CellRow> {
    indices.map(row).collect()
}

fn frame(rows: u32, cols: u32, seq: u64) -> CellGridFrame {
    CellGridFrame {
        stream_id: "stream-1".to_string(),
        grid_epoch: "epoch-1".to_string(),
        cols,
        rows,
        cursor_row: 3,
        cursor_col: 5,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: true,
        bracketed_paste: false,
        mouse_tracking: MouseTracking::None,
        mouse_sgr: false,
        focus_events: false,
        kitty_keyboard_flags: 0,
        full: true,
        viewport_rows: Vec::new(),
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 1000,
        sb_base: 1000,
        base_seq: 0,
        seq,
        image_placements: Some(roost_protocol::cell::no_image_placements()),
    }
}

fn projection(reader_intent: ReaderIntent) -> RendererProjection {
    RendererProjection {
        canonical: Some(Rc::new(frame(24, 80, 9))),
        applied: Some(Rc::new(frame(24, 80, 8))),
        canonical_watermark: RendererEpochSeq {
            grid_epoch: Some("epoch-1".to_string()),
            seq: Some(9),
        },
        reconciled_watermark: RendererEpochSeq {
            grid_epoch: Some("epoch-1".to_string()),
            seq: Some(8),
        },
        reader_intent,
        reader_reason: None,
        reader_anchor: None,
        hold_mask: 0,
        dom_rows: 24,
        reconciled_mode: Some(RendererTerminalModeSnapshot {
            alt_screen: false,
            cursor_keys_app: true,
            bracketed_paste: false,
        }),
        painted_cursor_visible: Some(true),
        painted_cursor_row: 3,
        painted_cursor_col: 5,
        cursor_connected: true,
        painted_cols: Some(80),
        at_bottom: true,
        follows_bottom: true,
        painted_history: painted(0..4),
        painted_sb_base: 0,
        scrollback_layout_end: 1000,
        painted_spacer_height: "0.00px".to_string(),
        gap_rows: 20,
        row_height: 16.0,
        default_row_height: 16.8,
        scroll_top: 0.0,
        scroll_height: 400.0,
        client_height: 400.0,
    }
}

#[test]
fn a_paint_presentation_reports_the_reserved_pixels_on_both_sides_of_the_painted_rows() {
    let presented =
        create_renderer_paint_presentation(&painted(10..14), "160.00px", 20, 16.0, None, None);
    assert_eq!(presented.head_spacer_px, 160.0);
    assert_eq!(presented.tail_gap_px, 320.0);
    assert_eq!(presented.rows.len(), 4);
    assert_eq!(presented.rows[0].index, 10);
    assert_eq!(presented.rows[0].text, "row 10");
    assert_eq!(presented.reader_anchor, None);
}

#[test]
fn an_unpainted_spacer_reads_as_zero_rather_than_as_nan() {
    let presented = create_renderer_paint_presentation(&[], "", 0, 0.0, None, None);
    assert_eq!(presented.head_spacer_px, 0.0);
    assert!(presented.tail_gap_px.is_finite());
    assert!(presented.rows.is_empty());
}

#[test]
fn the_paint_window_follows_the_reader_when_the_history_is_longer_than_it() {
    let history = painted(0..2000);
    let anchor = ReaderAnchor {
        row: 1500,
        offset_px: 4.0,
    };
    let presented =
        create_renderer_paint_presentation(&history, "0.00px", 0, 16.0, Some(anchor), Some(64));
    assert_eq!(presented.rows.len(), 64);
    assert!(presented.rows.iter().any(|painted| painted.index == 1500));
    assert_eq!(presented.reader_anchor, Some(anchor));
}

#[test]
fn the_paint_window_is_the_newest_rows_when_the_reader_is_not_parked() {
    let history = painted(0..2000);
    let presented = create_renderer_paint_presentation(&history, "0.00px", 0, 16.0, None, Some(64));
    let first = presented.rows.first().map(|painted| painted.index);
    let last = presented.rows.last().map(|painted| painted.index);
    assert_eq!(first, Some(1936));
    assert_eq!(last, Some(1999));
}

#[test]
fn a_snapshot_names_the_gap_between_canonical_and_reconciled() {
    let snapshot: RendererPresentationSnapshot =
        create_renderer_presentation_snapshot(&projection(ReaderIntent::Live), 1234.5);
    assert_eq!(snapshot.captured_at_ms, 1234.5);
    assert_eq!(snapshot.canonical.seq, Some(9));
    assert_eq!(snapshot.reconciled.seq, Some(8));
    assert_eq!(snapshot.canonical_rows, Some(24));
    assert_eq!(snapshot.dom_rows, 24);
    assert_eq!(snapshot.canonical_cols, Some(80));
    assert_eq!(snapshot.painted_cols, Some(80));
    assert!(snapshot.cursor_connected);
    assert!(snapshot.at_bottom);
    assert!(snapshot.follows_bottom);
    assert!(!snapshot.hold_mask_selection);
    assert!(!snapshot.hold_mask_link);
}

#[test]
fn a_snapshot_reports_a_parked_reader_and_its_reason() {
    let mut state = projection(ReaderIntent::Reading);
    state.reader_reason = Some(ReaderIntentReason::Find);
    state.at_bottom = false;
    state.follows_bottom = false;
    let snapshot = create_renderer_presentation_snapshot(&state, 0.0);
    assert_eq!(snapshot.reader_intent, ReaderIntent::Reading);
    assert_eq!(snapshot.reader_reason, Some(ReaderIntentReason::Find));
    assert!(!snapshot.at_bottom);
    assert!(!snapshot.follows_bottom);
}

#[test]
fn an_unreconciled_pane_reports_no_reconciled_mode_rather_than_a_guessed_one() {
    let mut state = projection(ReaderIntent::Live);
    state.reconciled_mode = None;
    let snapshot = create_renderer_presentation_snapshot(&state, 0.0);
    assert_eq!(snapshot.reconciled_mode, None);
    assert_eq!(
        snapshot.canonical_mode,
        Some(RendererTerminalModeSnapshot {
            alt_screen: false,
            cursor_keys_app: true,
            bracketed_paste: false,
        })
    );
}

#[test]
fn a_hidden_cursor_reports_no_row_and_no_column_rather_than_a_stale_one() {
    let mut state = projection(ReaderIntent::Live);
    state.painted_cursor_visible = Some(false);
    let snapshot = create_renderer_presentation_snapshot(&state, 0.0);
    assert_eq!(snapshot.painted_cursor_visible, Some(false));
    assert_eq!(snapshot.painted_cursor_row, None);
    assert_eq!(snapshot.painted_cursor_col, None);
    assert_eq!(snapshot.canonical_cursor, Some((true, 3, 5)));
}

#[test]
fn a_prediction_painted_at_the_tail_publishes_the_column_the_painter_intended() {
    let mut state = projection(ReaderIntent::Live);
    state.painted_cursor_col = 9;
    let snapshot = create_renderer_presentation_snapshot(&state, 0.0);
    assert_eq!(snapshot.painted_cursor_col, Some(9));
    assert_eq!(snapshot.canonical_cursor, Some((true, 3, 5)));
}

#[test]
fn the_same_painted_row_is_recognised_across_an_independent_decode() {
    let left = row(4);
    let right = row(4);
    assert!(same_scrollback_row(&left, &right));
    let different_text = CellRow {
        index: 4,
        mark: 0,
        spans: Arc::from([CellSpan {
            text: "other".to_string(),
            fg: 7,
            bg: 256,
            flags: 0,
            fg_rgb: None,
            bg_rgb: None,
            columns: 8,
            link_uri: None,
            link_key: None,
        }]),
    };
    assert!(!same_scrollback_row(&left, &different_text));
    let marked = CellRow {
        mark: roost_protocol::cell::row_mark::PROMPT,
        ..left.clone()
    };
    assert!(!same_scrollback_row(&left, &marked));
    assert!(!same_scrollback_row(&left, &row(5)));
}
