//! Column-geometry and decode-contract behaviour of the cell value model.
//!
//! The fixtures are what a real core emits: a narrow run, a width-2 lead with
//! its continuation folded in, an astral codepoint, and a multi-scalar grapheme
//! cluster. Every assertion here is a property a renderer, a find highlight or a
//! decode boundary depends on.

use std::sync::Arc;

use roost_protocol::cell::types::{
    CellRow, CellSpan, DEFAULT_COLOR, MAX_LINK_URI_BYTES, MouseTracking, as_mouse_tracking,
    assert_cell_row_spans, column_span, column_text, link_uri_within_cap, row_columns,
    span_is_atomic, spans_text, text_offset_to_column, text_offset_to_column_end,
    text_range_to_columns,
};

fn span(text: &str, columns: u32) -> CellSpan {
    CellSpan {
        text: text.to_owned(),
        fg: DEFAULT_COLOR,
        bg: DEFAULT_COLOR,
        flags: 0,
        fg_rgb: None,
        bg_rgb: None,
        columns,
        link_uri: None,
        link_key: None,
    }
}

fn linked(text: &str, uri: &str, key: Option<&str>) -> CellSpan {
    CellSpan {
        link_uri: Some(uri.to_owned()),
        link_key: key.map(str::to_owned),
        ..span(text, text.chars().count() as u32)
    }
}

/// "ab中文cd" as the core ships it: a narrow run, two width-2 leads, a run.
fn mixed_row() -> Vec<CellSpan> {
    vec![span("ab", 2), span("中", 2), span("文", 2), span("cd", 2)]
}

fn cell_row(index: u32, spans: Vec<CellSpan>) -> CellRow {
    CellRow {
        index,
        spans: Arc::from(spans),
    }
}

fn span_at(spans: &[CellSpan], col: i64) -> (&str, u32) {
    match column_span(spans, col) {
        Some((span, offset)) => (span.text.as_str(), offset),
        None => panic!("no span covers column {col}"),
    }
}

fn decode_fault(row: &CellRow, max_columns: u32) -> String {
    match assert_cell_row_spans(row, max_columns) {
        Err(fault) => fault.reason,
        Ok(()) => panic!("row {} must be refused", row.index),
    }
}

#[test]
fn text_offsets_map_to_the_columns_the_paint_used() {
    let spans = mixed_row();
    assert_eq!(spans_text(&spans), "ab中文cd");
    assert_eq!(row_columns(&spans), 8);
    let mapped: Vec<u32> = (0..6).map(|at| text_offset_to_column(&spans, at)).collect();
    assert_eq!(mapped, vec![0, 1, 2, 4, 6, 7]);
    // Past the end resolves to the row's width, not to a clamped last column.
    assert_eq!(text_offset_to_column(&spans, 6), 8);
}

#[test]
fn a_match_covers_whole_glyphs_never_half_of_one() {
    let spans = mixed_row();
    // "中文" is scalars 2..4: four grid columns rather than two.
    let wide = text_range_to_columns(&spans, 2, 2);
    assert_eq!((wide.col, wide.columns), (2, 4));
    // "b中" runs from column 1 through the end of 中.
    let straddling = text_range_to_columns(&spans, 1, 2);
    assert_eq!((straddling.col, straddling.columns), (1, 3));
    // A narrow-only match keeps the 1:1 identity.
    let narrow = text_range_to_columns(&spans, 4, 2);
    assert_eq!((narrow.col, narrow.columns), (6, 2));
    let empty = text_range_to_columns(&spans, 0, 0);
    assert_eq!((empty.col, empty.columns), (0, 0));
}

#[test]
fn column_lookups_read_the_glyph_occupying_that_column() {
    let spans = mixed_row();
    let painted: Vec<&str> = (0..9).map(|col| column_text(&spans, col)).collect();
    assert_eq!(
        painted,
        vec!["a", "b", "中", "中", "文", "文", "c", "d", ""]
    );
    // No column before the row, and none past its end.
    assert!(column_span(&spans, -1).is_none());
    assert!(column_span(&spans, 8).is_none());
    assert_eq!(span_at(&spans, 1), ("ab", 1));
    // A wide glyph's continuation column resolves to its LEAD span, offset 1.
    assert_eq!(span_at(&spans, 3), ("中", 1));
}

#[test]
fn a_narrow_run_is_not_atomic_and_a_wide_glyph_is() {
    assert!(!span_is_atomic(&span("ab", 2)));
    assert!(span_is_atomic(&span("中", 2)));
    assert!(!span_is_atomic(&span("a", 1)));
}

#[test]
fn an_astral_codepoint_is_atomic_at_any_column_count() {
    let narrow = span("🐙", 1);
    let wide = span("🐙", 2);
    // One column: the scalar count agrees with the column count, and only the
    // above-BMP test catches it. Two columns: the counts disagree.
    assert!(span_is_atomic(&narrow));
    assert!(span_is_atomic(&wide));
    assert_eq!(column_text(&[narrow], 0), "🐙");
    assert_eq!(column_text(std::slice::from_ref(&wide), 1), "🐙");
    // Its one scalar never advances the text offset, yet the span owns both
    // columns, so a match ending past it covers the whole glyph.
    assert_eq!(text_offset_to_column(std::slice::from_ref(&wide), 0), 0);
    assert_eq!(text_offset_to_column_end(&[wide], 1), 2);
}

#[test]
fn a_grapheme_cluster_is_one_cell_of_many_scalars() {
    let cluster = span("e\u{301}\u{200d}\u{1f469}", 1);
    assert_eq!(cluster.text.chars().count(), 4);
    assert!(span_is_atomic(&cluster));
    // The whole cluster paints at its one column and covers a whole match.
    assert_eq!(column_text(std::slice::from_ref(&cluster), 0), cluster.text);
    let covered = text_range_to_columns(&[cluster], 0, 2);
    assert_eq!((covered.col, covered.columns), (0, 1));
}

#[test]
fn a_span_whose_columns_disagree_with_its_text_is_never_sliced() {
    // A producer that ships the wrong occupancy would otherwise have the paint
    // read one character of a three-character run.
    let mistyped = span("abc", 5);
    assert!(span_is_atomic(&mistyped));
    assert_eq!(column_text(&[mistyped], 4), "abc");
    let overclaimed = span("ab", 1);
    assert!(span_is_atomic(&overclaimed));
    assert_eq!(row_columns(&[overclaimed]), 1);
}

#[test]
fn an_empty_row_has_no_columns_and_no_text() {
    let empty: Vec<CellSpan> = Vec::new();
    assert_eq!(row_columns(&empty), 0);
    assert_eq!(spans_text(&empty), "");
    assert_eq!(text_offset_to_column(&empty, 0), 0);
    assert_eq!(text_offset_to_column_end(&empty, 0), 0);
    assert_eq!(column_text(&empty, 0), "");
    assert!(column_span(&empty, 0).is_none());
    assert!(assert_cell_row_spans(&cell_row(0, empty), 0).is_ok());
}

#[test]
fn a_link_uri_is_capped_in_utf8_bytes_at_the_boundary() {
    assert!(link_uri_within_cap(&"a".repeat(MAX_LINK_URI_BYTES)));
    assert!(!link_uri_within_cap(&"a".repeat(MAX_LINK_URI_BYTES + 1)));
    // 3 bytes each: 682 characters fit, 683 do not.
    assert!(link_uri_within_cap(&"中".repeat(682)));
    assert!(!link_uri_within_cap(&"中".repeat(683)));
    // 4 bytes each: exactly 512 fit, 513 do not.
    assert!(link_uri_within_cap(&"🐙".repeat(MAX_LINK_URI_BYTES / 4)));
    assert!(!link_uri_within_cap(
        &"🐙".repeat(MAX_LINK_URI_BYTES / 4 + 1)
    ));
}

#[test]
fn a_complete_link_passes_at_the_cap_and_fails_one_byte_past_it() {
    let at_cap = linked("t", &"h".repeat(MAX_LINK_URI_BYTES), Some("run-1"));
    assert!(assert_cell_row_spans(&cell_row(3, vec![at_cap]), 80).is_ok());
    let over_cap = linked("t", &"h".repeat(MAX_LINK_URI_BYTES + 1), Some("run-1"));
    let fault = decode_fault(&cell_row(3, vec![over_cap]), 80);
    assert!(fault.contains("over 2048 bytes"), "{fault}");
}

#[test]
fn half_a_link_is_a_decode_error_in_a_fixed_order() {
    let no_key = linked("ab", "https://example.test/x", None);
    let empty_uri = linked("ab", "", Some("run-1"));
    let key_alone = CellSpan {
        link_key: Some("run-1".to_owned()),
        ..span("ab", 2)
    };
    assert!(decode_fault(&cell_row(3, vec![no_key]), 80).contains("link_uri with no link_key"));
    assert!(decode_fault(&cell_row(3, vec![empty_uri]), 80).contains("empty link_uri"));
    assert!(decode_fault(&cell_row(3, vec![key_alone]), 80).contains("link_key with no link_uri"));
    // An empty URI is reported before the missing key it also has.
    let both_faults = linked("ab", "", None);
    assert!(decode_fault(&cell_row(3, vec![both_faults]), 80).contains("empty link_uri"));
}

#[test]
fn the_column_faults_precede_the_link_faults_and_name_the_row() {
    let no_columns = CellSpan {
        link_uri: Some(String::new()),
        ..span("ab", 0)
    };
    let fault = match assert_cell_row_spans(&cell_row(7, vec![no_columns]), 80) {
        Err(fault) => fault,
        Ok(()) => panic!("a span claiming no columns must be a decode error"),
    };
    assert!(
        fault.reason.contains("claims 0 columns"),
        "{}",
        fault.reason
    );
    assert_eq!(fault.field, "cell_row[7].spans[0].columns");

    let fault = match assert_cell_row_spans(&cell_row(7, vec![span("", 2)]), 80) {
        Err(fault) => fault,
        Ok(()) => panic!("a span with no text must be a decode error"),
    };
    assert!(fault.reason.contains("carries no text"), "{}", fault.reason);
    assert_eq!(fault.field, "cell_row[7].spans[0].text");
}

#[test]
fn a_row_wider_than_its_grid_is_rejected_but_retained_history_is_not() {
    let wide = vec![span("abcdefgh", 8)];
    let fault = decode_fault(&cell_row(1, wide.clone()), 6);
    assert!(fault.contains("8 columns of a 6-column grid"), "{fault}");
    // A retained scrollback line keeps its write-time width, so 0 means no bound.
    assert!(assert_cell_row_spans(&cell_row(1, wide), 0).is_ok());
}

#[test]
fn a_mouse_mode_this_build_does_not_name_still_decodes() {
    assert_eq!(as_mouse_tracking(0), MouseTracking::None);
    assert_eq!(as_mouse_tracking(1000), MouseTracking::PressRelease);
    assert_eq!(as_mouse_tracking(1002), MouseTracking::ButtonMotion);
    // The core folds 9 and 1003 itself; anything else reads as no tracking.
    assert_eq!(as_mouse_tracking(9), MouseTracking::None);
    assert_eq!(as_mouse_tracking(1003), MouseTracking::None);
    assert_eq!(as_mouse_tracking(1001), MouseTracking::None);
    // Decoding keeps the raw integer, so a future mode round-trips as itself.
    assert_eq!(MouseTracking::from(1003), MouseTracking::Unknown(1003));
    assert_eq!(u32::from(MouseTracking::Unknown(1003)), 1003);
    assert_eq!(u32::from(as_mouse_tracking(1002)), 1002);
}
