//! The pure half of row painting: the palette mapping, one span's inline CSS,
//! how a find match splits a run, and the row hash the viewport diff compares.
//!
//! Each rule here is one a browser would otherwise be the only oracle for. The
//! occupancy box is why a painted row is exactly `cols` wide; the decoration
//! half is why a highlighted span actually looks highlighted; the hash is why
//! an ordinary delta costs only its dirty rows.

use std::sync::Arc;

use roost_protocol::cell::{
    CELL_BOLD, CELL_REVERSE, CELL_STRIKE, CELL_UNDERLINE, CellRow, CellSpan,
};
use roost_web_terminal::cell_row::{
    FindHit, ansi256_to_css, row_column_count, row_hash, slice_text, span_decoration_style,
    span_slices, span_style,
};

fn span(text: &str, columns: u32) -> CellSpan {
    CellSpan {
        text: text.to_string(),
        fg: 7,
        bg: 256,
        flags: 0,
        fg_rgb: None,
        bg_rgb: None,
        columns,
        link_uri: None,
        link_key: None,
    }
}

fn row(spans: Vec<CellSpan>) -> CellRow {
    CellRow {
        index: 0,
        spans: Arc::from(spans),
    }
}

fn described(slices: &[roost_web_terminal::cell_row::SpanSlice]) -> Vec<(u32, u32, bool, bool)> {
    slices
        .iter()
        .map(|slice| (slice.start, slice.columns, slice.highlighted, slice.active))
        .collect()
}

#[test]
fn the_palette_maps_theme_vars_then_the_cube_then_the_grayscale_ramp() {
    assert_eq!(ansi256_to_css(0), "var(--term-color-0)");
    assert_eq!(ansi256_to_css(15), "var(--term-color-15)");
    assert_eq!(ansi256_to_css(16), "rgb(0,0,0)");
    assert_eq!(ansi256_to_css(21), "rgb(0,0,255)");
    // Cube arithmetic is `(index - 16)`: 35 is r=0, g=35/6=5, b=35%6=5 — cyan.
    assert_eq!(ansi256_to_css(51), "rgb(0,255,255)");
    assert_eq!(ansi256_to_css(232), "rgb(8,8,8)");
    assert_eq!(ansi256_to_css(255), "rgb(238,238,238)");
}

#[test]
fn a_default_background_is_not_emitted_so_the_container_shows_through() {
    assert_eq!(span_style(&span("hi", 2)), "color:var(--term-color-7)");
    let coloured = CellSpan {
        bg: 1,
        ..span("hi", 2)
    };
    assert_eq!(
        span_style(&coloured),
        "color:var(--term-color-7);background:var(--term-color-1)"
    );
}

#[test]
fn reverse_swaps_foreground_and_background_and_still_emits_a_background() {
    let reversed = CellSpan {
        fg: 2,
        bg: 3,
        flags: CELL_REVERSE,
        ..span("x", 1)
    };
    assert_eq!(
        span_style(&reversed),
        "color:var(--term-color-3);background:var(--term-color-2)"
    );
}

#[test]
fn a_true_color_span_is_painted_as_the_hex_the_core_sent() {
    let true_color = CellSpan {
        fg: 1,
        fg_rgb: Some(0x00a0_b0c0),
        ..span("x", 1)
    };
    assert!(
        span_style(&true_color).contains("color:#a0b0c0"),
        "{}",
        span_style(&true_color)
    );
}

#[test]
fn the_decoration_half_carries_no_colour_so_a_find_class_can_own_it() {
    let styled = CellSpan {
        flags: CELL_BOLD | CELL_UNDERLINE | CELL_STRIKE,
        ..span("x", 1)
    };
    let decoration = span_decoration_style(&styled);
    assert!(!decoration.contains("color"), "{decoration}");
    assert!(!decoration.contains("background"), "{decoration}");
    assert!(decoration.contains("font-weight:bold"), "{decoration}");
    assert!(
        decoration.contains("text-decoration:underline line-through"),
        "{decoration}"
    );
}

#[test]
fn an_atomic_span_pins_its_box_to_its_declared_columns() {
    let wide = span("\u{4e2d}", 2);
    let decoration = span_decoration_style(&wide);
    assert!(
        decoration.contains("display:inline-block;width:2ch"),
        "{decoration}"
    );
    assert!(
        !span_decoration_style(&span("ab", 2)).contains("inline-block"),
        "a coalesced narrow run needs no box"
    );
}

#[test]
fn a_narrow_run_splits_at_every_match_boundary() {
    let run = span("abcdef", 6);
    let slices = span_slices(
        &run,
        0,
        &[FindHit { col: 1, len: 2 }, FindHit { col: 4, len: 1 }],
        Some(4),
    );
    assert_eq!(
        described(&slices),
        vec![
            (0, 1, false, false),
            (1, 2, true, false),
            (3, 1, false, false),
            (4, 1, true, true),
            (5, 1, false, false),
        ]
    );
    let text: String = slices
        .iter()
        .map(|slice| slice_text(&run, slice.start, slice.columns))
        .collect();
    assert_eq!(text, "abcdef");
}

#[test]
fn a_match_running_past_the_end_never_produces_an_empty_tail() {
    let run = span("abcd", 4);
    let slices = span_slices(&run, 0, &[FindHit { col: 2, len: 9 }], None);
    assert_eq!(
        described(&slices),
        vec![(0, 2, false, false), (2, 2, true, false)]
    );
    let text: String = slices
        .iter()
        .map(|slice| slice_text(&run, slice.start, slice.columns))
        .collect();
    assert_eq!(text, "abcd");
}

#[test]
fn an_atomic_span_is_never_cut_by_a_column_boundary() {
    let wide = span("\u{4e2d}", 2);
    let slices = span_slices(&wide, 0, &[FindHit { col: 1, len: 1 }], Some(1));
    assert_eq!(described(&slices), vec![(0, 2, true, true)]);
    assert_eq!(
        slice_text(&wide, slices[0].start, slices[0].columns),
        "\u{4e2d}"
    );
}

#[test]
fn an_unmatched_span_paints_as_one_unhighlighted_slice() {
    let run = span("abc", 3);
    let slices = span_slices(&run, 0, &[FindHit { col: 9, len: 1 }], None);
    assert_eq!(described(&slices), vec![(0, 3, false, false)]);
}

#[test]
fn a_span_no_find_state_at_all_paints_as_its_own_text() {
    let run = span("\u{1f419}", 1);
    assert_eq!(slice_text(&run, 0, 1), "\u{1f419}");
}

#[test]
fn the_row_hash_distinguishes_exactly_what_the_painter_paints() {
    let base = row(vec![span("abc", 3)]);
    let hits = [FindHit { col: 1, len: 1 }];
    assert_eq!(row_hash(&base, None, None), row_hash(&base, None, None));
    assert_ne!(
        row_hash(&base, None, None),
        row_hash(&base, Some(&hits), None)
    );
    assert_ne!(
        row_hash(&base, Some(&hits), None),
        row_hash(&base, Some(&hits), Some(1))
    );
    // The same text at a different declared width is a different painted row.
    assert_ne!(
        row_hash(&base, None, None),
        row_hash(&row(vec![span("abc", 4)]), None, None)
    );
    // So is a different split of the same text.
    let split = row(vec![span("a", 1), span("bc", 2)]);
    assert_ne!(row_hash(&base, None, None), row_hash(&split, None, None));
    // So is a different flag bit on the same cells.
    let bold = row(vec![CellSpan {
        flags: CELL_BOLD,
        ..span("abc", 3)
    }]);
    assert_ne!(row_hash(&base, None, None), row_hash(&bold, None, None));
}

#[test]
fn the_row_hash_folds_link_identity_without_hashing_the_whole_uri() {
    let plain = row(vec![span("x", 1)]);
    let painted = CellSpan {
        link_uri: Some("https://a.dev/".to_string()),
        link_key: Some("k1".to_string()),
        ..span("x", 1)
    };
    let other_key = CellSpan {
        link_uri: Some("https://a.dev/".to_string()),
        link_key: Some("k2".to_string()),
        ..span("x", 1)
    };
    let longer_uri = CellSpan {
        link_uri: Some("https://a.dev/very/long/path".to_string()),
        link_key: Some("k1".to_string()),
        ..span("x", 1)
    };
    assert_ne!(
        row_hash(&plain, None, None),
        row_hash(&row(vec![painted.clone()]), None, None)
    );
    assert_ne!(
        row_hash(&row(vec![painted.clone()]), None, None),
        row_hash(&row(vec![other_key]), None, None)
    );
    assert_ne!(
        row_hash(&row(vec![painted]), None, None),
        row_hash(&row(vec![longer_uri]), None, None)
    );
}

#[test]
fn an_astral_scalar_hashes_by_code_unit_so_two_of_them_separate() {
    let one = row(vec![span("\u{1f419}", 1)]);
    let two = row(vec![span("\u{1f419}\u{1f419}", 2)]);
    assert_ne!(row_hash(&one, None, None), row_hash(&two, None, None));
}

#[test]
fn the_stamped_grid_occupancy_is_columns_never_code_units() {
    let wide = row(vec![span("\u{4e2d}\u{6587}", 4)]);
    assert_eq!(row_column_count(&wide), 4);
    assert_eq!(
        wide.spans.iter().next().map(|span| span.text.len()),
        Some(6)
    );
}
