//! Wide-glyph COLUMN OCCUPANCY in the painted row, ported from
//! `apps/web/tests/cellRowWide.dom.test.ts`: the paint derives geometry from
//! `CellSpan::columns`, never from text length, so no phantom space appears,
//! find hits land on the cells the worker matched, and no glyph is cut.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use std::sync::Arc;

use render_support::FakeEl;
use roost_protocol::cell::{CellRow, CellSpan, DEFAULT_COLOR, row_columns};
use roost_web_terminal::RenderElement;
use roost_web_terminal::cell_row::dom::render_row;
use roost_web_terminal::cell_row::style_cache::StyleCache;
use roost_web_terminal::cell_row::{FindHit, row_hash, span_style};

fn atom(text: &str, columns: u32) -> CellSpan {
    CellSpan {
        text: text.to_string(),
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

fn run(text: &str) -> CellSpan {
    atom(text, u32::try_from(text.encode_utf16().count()).unwrap())
}

fn row_of(spans: Vec<CellSpan>) -> CellRow {
    CellRow {
        index: 0,
        spans: Arc::from(spans),
    }
}

fn paint(row: &CellRow, hits: Option<&[FindHit]>, active_col: Option<u32>) -> FakeEl {
    render_row(
        row,
        &FakeEl::new("div"),
        hits,
        active_col,
        &mut StyleCache::default(),
    )
    .unwrap()
}

/// A pinned box counts its declared `ch` width; an unboxed narrow run counts
/// one column per code unit.
fn piece_columns(piece: &FakeEl) -> usize {
    let style = piece.attribute("style").unwrap_or_default();
    style
        .split(';')
        .find_map(|declaration| {
            declaration
                .strip_prefix("width:")?
                .strip_suffix("ch")?
                .parse()
                .ok()
        })
        .unwrap_or_else(|| piece.text_content().encode_utf16().count())
}

fn painted_columns(element: &FakeEl) -> usize {
    element.children().iter().map(piece_columns).sum()
}

fn piece_start_columns(element: &FakeEl) -> Vec<usize> {
    let mut column = 0;
    element
        .children()
        .iter()
        .map(|piece| {
            let start = column;
            column += piece_columns(piece);
            start
        })
        .collect()
}

fn painted_pieces(element: &FakeEl) -> Vec<(String, String)> {
    element
        .children()
        .iter()
        .map(|piece| (piece.text_content(), piece.class_name()))
        .collect()
}

fn pieces(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(text, class)| (text.to_string(), class.to_string()))
        .collect()
}

#[test]
fn a_cjk_row_paints_its_grid_columns_and_carries_no_phantom_spaces() {
    let row = row_of(vec![atom("中", 2), atom("文", 2), run(" ok")]);
    assert_eq!(row_columns(&row.spans), 7);
    let element = paint(&row, None, None);
    assert_eq!(painted_columns(&element), 7);
    assert_eq!(element.text_content(), "中文 ok");
}

#[test]
fn an_atomic_span_is_pinned_to_its_column_box_and_a_narrow_run_is_not() {
    assert!(span_style(&atom("中", 2)).contains("display:inline-block;width:2ch"));
    assert!(span_style(&atom("\u{1F1FA}", 1)).contains("display:inline-block;width:1ch"));
    assert!(!span_style(&run("ok")).contains("width"));
}

#[test]
fn emoji_with_a_zwj_sequence_and_a_skin_tone_modifier_keep_every_glyph_whole() {
    let row = row_of(vec![
        atom("🐙", 2),
        run(" "),
        atom("👨", 2),
        run("\u{200d}"),
        atom("👩", 2),
        run("\u{200d}"),
        atom("👧", 2),
        run(" "),
        atom("👋", 2),
        atom("🏽", 2),
    ]);
    assert_eq!(row_columns(&row.spans), 16);
    let element = paint(&row, Some(&[FindHit { col: 4, len: 4 }]), Some(4));
    assert_eq!(painted_columns(&element), 16);
    assert_eq!(element.text_content(), "🐙 👨\u{200d}👩\u{200d}👧 👋🏽");
}

#[test]
fn a_joined_cluster_in_one_atomic_span_paints_every_scalar() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let row = row_of(vec![atom(family, 2), run("x")]);
    assert_eq!(paint(&row, None, None).text_content(), format!("{family}x"));
    let highlighted = paint(&row, Some(&[FindHit { col: 1, len: 1 }]), Some(1));
    assert_eq!(
        painted_pieces(&highlighted)[0],
        (
            family.to_string(),
            "cell-find-hit cell-find-hit-active".to_string()
        )
    );
}

#[test]
fn a_hit_that_touches_a_wide_glyph_highlights_the_whole_glyph() {
    let row = row_of(vec![run("ab"), atom("中", 2), run("cd")]);
    let element = paint(&row, Some(&[FindHit { col: 1, len: 2 }]), None);
    assert_eq!(
        painted_pieces(&element),
        pieces(&[
            ("a", ""),
            ("b", "cell-find-hit"),
            ("中", "cell-find-hit"),
            ("cd", "")
        ])
    );
    assert_eq!(painted_columns(&element), 6);
}

#[test]
fn hit_columns_past_a_wide_glyph_land_on_the_characters_the_worker_matched() {
    let row = row_of(vec![atom("中", 2), run("abc")]);
    let element = paint(&row, Some(&[FindHit { col: 3, len: 2 }]), None);
    assert_eq!(
        painted_pieces(&element),
        pieces(&[("中", ""), ("a", ""), ("bc", "cell-find-hit")])
    );
    assert_eq!(piece_start_columns(&element), vec![0, 2, 3]);
}

#[test]
fn the_active_hit_keeps_its_own_class_across_a_wide_glyph() {
    let row = row_of(vec![run("x"), atom("文", 2), run("y")]);
    let active = paint(&row, Some(&[FindHit { col: 1, len: 2 }]), Some(1));
    assert_eq!(
        painted_pieces(&active)[1],
        (
            "文".to_string(),
            "cell-find-hit cell-find-hit-active".to_string()
        )
    );
    let other = paint(&row, Some(&[FindHit { col: 1, len: 2 }]), Some(9));
    assert_eq!(other.children()[1].class_name(), "cell-find-hit");
}

#[test]
fn every_painted_piece_starts_at_the_grid_column_the_model_gives_it() {
    let row = row_of(vec![
        run("ab"),
        atom("中", 2),
        run("c"),
        atom("👋", 2),
        run("d"),
    ]);
    let element = paint(&row, None, None);
    assert_eq!(piece_start_columns(&element), vec![0, 2, 4, 5, 7]);
    assert_eq!(painted_columns(&element), 8);
    assert_eq!(row_columns(&row.spans), 8);
}

#[test]
fn row_identity_folds_occupancy_so_the_same_text_at_a_different_width_repaints() {
    let as_run = row_of(vec![run("中中")]);
    let as_wide = row_of(vec![atom("中", 2), atom("中", 2)]);
    assert_ne!(
        row_hash(&as_run, None, None),
        row_hash(&as_wide, None, None)
    );
}
