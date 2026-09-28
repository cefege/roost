//! Canonical terminal-state comparison. The whole attribution chain rests on
//! "are these two layers holding the same screen?": a false equal exonerates
//! the layer that broke, a false different blames one that merely re-encoded
//! the same cells. Ports `packages/protocol/tests/terminal-capture-view.test.ts`
//! (the painted-row fingerprint cases stay with the browser, which owns them).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::cell::{
    CELL_BOLD, CellGridFrame, CellRow, CellSpan, DEFAULT_COLOR, MouseTracking,
};
use roost_protocol::terminal_capture::view::{
    TerminalCanonicalDifference, TerminalCanonicalField, TerminalCanonicalView, TerminalCellField,
    canonical_view_of_frame, compare_canonical_views,
};

fn span(text: &str) -> CellSpan {
    CellSpan {
        text: text.to_owned(),
        fg: DEFAULT_COLOR,
        bg: DEFAULT_COLOR,
        flags: 0,
        fg_rgb: None,
        bg_rgb: None,
        columns: text.chars().count() as u32,
        link_uri: None,
        link_key: None,
    }
}

fn wide(text: &str, columns: u32) -> CellSpan {
    CellSpan {
        columns,
        ..span(text)
    }
}

fn row(index: u32, spans: Vec<CellSpan>) -> CellRow {
    CellRow {
        index,
        spans: spans.into(),
    }
}

fn frame(rows: Vec<CellRow>) -> CellGridFrame {
    CellGridFrame {
        stream_id: "11111111-1111-4111-8111-111111111111".to_owned(),
        grid_epoch: "epoch:1".to_owned(),
        cols: 10,
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
        viewport_rows: rows,
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq: 0,
        seq: 1,
    }
}

fn view_of(frame: &CellGridFrame) -> TerminalCanonicalView<'_> {
    canonical_view_of_frame(frame).expect("a dense full is a canonical view")
}

fn compare(left: &CellGridFrame, right: &CellGridFrame) -> Option<TerminalCanonicalDifference> {
    compare_canonical_views(&view_of(left), &view_of(right))
}

fn cell_field(difference: Option<TerminalCanonicalDifference>) -> (u32, u32, TerminalCellField) {
    match difference {
        Some(TerminalCanonicalDifference::Row {
            row, column, field, ..
        }) => (row, column, field),
        other => panic!("expected a cell difference, got {other:?}"),
    }
}

fn state_field(difference: Option<TerminalCanonicalDifference>) -> TerminalCanonicalField {
    match difference {
        Some(TerminalCanonicalDifference::State { field, .. }) => field,
        other => panic!("expected a state difference, got {other:?}"),
    }
}

#[test]
fn a_sparse_delta_is_never_a_canonical_view() {
    let mut delta = frame(vec![row(1, vec![span("x")])]);
    (delta.full, delta.rows, delta.base_seq, delta.seq) = (false, 3, 1, 2);
    assert!(canonical_view_of_frame(&delta).is_none());
}

#[test]
fn a_full_whose_viewport_is_not_densely_index_ordered_is_refused() {
    let misnumbered = frame(vec![row(0, vec![span("a")]), row(2, vec![span("b")])]);
    assert!(canonical_view_of_frame(&misnumbered).is_none());
}

#[test]
fn delivery_framing_and_history_payloads_do_not_differ() {
    let mut left = frame(vec![row(0, vec![span("FOOTER 12s")])]);
    left.scrollback_rows = vec![row(4, vec![span("old")])];
    let mut right = frame(vec![row(0, vec![span("FOOTER 12s")])]);
    right.scrollback_append = vec![row(9, vec![span("newer")])];
    right.base_seq = 7;
    assert_eq!(compare(&left, &right), None);
}

#[test]
fn a_different_but_legal_span_split_of_the_same_cells_is_equal() {
    let coalesced = frame(vec![row(0, vec![span("abcd")])]);
    let resplit = frame(vec![row(0, vec![span("ab"), span("cd")])]);
    assert_eq!(compare(&coalesced, &resplit), None);
}

#[test]
fn one_differing_character_names_its_row_and_column() {
    let left = frame(vec![row(0, vec![span("FOOTER 12s")])]);
    let right = frame(vec![row(0, vec![span("FOOTER 14s")])]);
    assert_eq!(
        cell_field(compare(&left, &right)),
        (0, 8, TerminalCellField::Text)
    );
}

#[test]
fn a_style_only_divergence_is_as_loud_as_a_text_one() {
    let left = frame(vec![row(0, vec![span("ok")])]);
    let right = frame(vec![row(
        0,
        vec![CellSpan {
            flags: CELL_BOLD,
            ..span("ok")
        }],
    )]);
    assert_eq!(
        cell_field(compare(&left, &right)),
        (0, 0, TerminalCellField::Flags)
    );
}

#[test]
fn a_wide_glyph_is_aligned_by_column_not_by_code_unit() {
    let leading = frame(vec![row(0, vec![wide("中", 2), span("x")])]);
    let trailing = frame(vec![row(0, vec![span("x"), wide("中", 2)])]);
    assert_eq!(
        cell_field(compare(&leading, &trailing)),
        (0, 0, TerminalCellField::Text)
    );
}

#[test]
fn a_row_claiming_different_total_columns_is_reported_as_occupancy() {
    let three = frame(vec![row(0, vec![wide("中", 2), span("x")])]);
    let two = frame(vec![row(0, vec![wide("中x", 2)])]);
    match compare(&three, &two) {
        Some(TerminalCanonicalDifference::Row {
            row: 0,
            field: TerminalCellField::RowColumns,
            left,
            right,
            ..
        }) => {
            assert_eq!((left.as_str(), right.as_str()), ("3", "2"));
        }
        other => panic!("expected an occupancy difference, got {other:?}"),
    }
}

#[test]
fn a_link_on_identical_text_is_a_divergence() {
    let plain = frame(vec![row(0, vec![span("docs")])]);
    let linked_span = CellSpan {
        link_uri: Some("https://example.test/a".to_owned()),
        link_key: Some("k1".to_owned()),
        ..span("docs")
    };
    let linked = frame(vec![row(0, vec![linked_span])]);
    assert_eq!(
        cell_field(compare(&plain, &linked)).2,
        TerminalCellField::LinkUri
    );
}

#[test]
fn cursor_mode_and_scrollback_bound_fields_are_compared_by_name() {
    let base = frame(vec![row(0, vec![span("a")])]);
    let cases: Vec<(fn(&mut CellGridFrame), TerminalCanonicalField)> = vec![
        (|f| f.cursor_col = 3, TerminalCanonicalField::CursorCol),
        (
            |f| f.cursor_visible = false,
            TerminalCanonicalField::CursorVisible,
        ),
        (|f| f.alt_screen = true, TerminalCanonicalField::AltScreen),
        (
            |f| f.cursor_keys_app = true,
            TerminalCanonicalField::CursorKeysApp,
        ),
        (
            |f| f.bracketed_paste = true,
            TerminalCanonicalField::BracketedPaste,
        ),
        (
            |f| f.mouse_tracking = MouseTracking::ButtonMotion,
            TerminalCanonicalField::MouseTracking,
        ),
        (|f| f.mouse_sgr = true, TerminalCanonicalField::MouseSgr),
        (
            |f| f.focus_events = true,
            TerminalCanonicalField::FocusEvents,
        ),
        (
            |f| (f.scrollback_total, f.sb_base) = (5, 5),
            TerminalCanonicalField::SbBase,
        ),
    ];
    for (change, field) in cases {
        let mut changed = frame(vec![row(0, vec![span("a")])]);
        change(&mut changed);
        assert_eq!(state_field(compare(&base, &changed)), field);
    }
}

#[test]
fn a_differing_row_count_is_reported_as_geometry_first() {
    let shorter = frame(vec![row(0, vec![span("a")])]);
    let longer = frame(vec![row(0, vec![span("a")]), row(1, vec![span("b")])]);
    // `rows` is compared before the row list, so the geometry field wins.
    assert_eq!(
        state_field(compare(&shorter, &longer)),
        TerminalCanonicalField::Rows
    );
}

#[test]
fn no_field_of_the_difference_carries_the_rows_characters() {
    let left = frame(vec![row(0, vec![span("secret-token-value")])]);
    let right = frame(vec![row(0, vec![span("secret-token-walue")])]);
    let serialized = serde_json::to_string(&compare(&left, &right)).unwrap();
    assert!(!serialized.contains("secret"), "{serialized}");
    assert!(!serialized.contains("token"), "{serialized}");
    assert!(serialized.contains(r#""kind":"row""#), "{serialized}");
}
