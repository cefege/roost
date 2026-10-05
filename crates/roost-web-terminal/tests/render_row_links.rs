//! OSC 8 hyperlinks in the painted row, ported from
//! `apps/web/tests/cellRowLinks.dom.test.ts`: links are core-authored per span,
//! so the paint must keep differently-keyed runs apart, keep a find-split run
//! inside its one anchor, refuse unsafe targets, and stamp grid COLUMNS.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use std::sync::Arc;

use render_support::FakeEl;
use roost_protocol::cell::{CellRow, CellSpan, DEFAULT_COLOR};
use roost_web_terminal::RenderElement;
use roost_web_terminal::cell_row::dom::render_row;
use roost_web_terminal::cell_row::style_cache::StyleCache;
use roost_web_terminal::cell_row::{
    FindHit, LINK_KEY_ATTR, ROW_COLUMNS_ATTR, ROW_HAS_LINKS_ATTR, TERMINAL_LINK_CLASS, row_hash,
};

fn run_of(text: &str, columns: u32) -> CellSpan {
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
    run_of(text, u32::try_from(text.encode_utf16().count()).unwrap())
}

fn linked(text: &str, uri: &str, key: &str) -> CellSpan {
    CellSpan {
        link_uri: Some(uri.to_string()),
        link_key: Some(key.to_string()),
        ..run(text)
    }
}

fn row(spans: Vec<CellSpan>) -> CellRow {
    CellRow {
        index: 0,
        spans: Arc::from(spans),
    }
}

fn paint(spans: Vec<CellSpan>, hits: Option<&[FindHit]>) -> FakeEl {
    render_row(
        &row(spans),
        &FakeEl::new("div"),
        hits,
        None,
        &mut StyleCache::default(),
    )
    .unwrap()
}

fn anchors_of(element: &FakeEl) -> Vec<FakeEl> {
    element
        .children()
        .into_iter()
        .filter(|child| child.tag() == "a")
        .collect()
}

fn child_texts(element: &FakeEl) -> Vec<String> {
    element
        .children()
        .iter()
        .map(FakeEl::text_content)
        .collect()
}

#[test]
fn a_linked_span_paints_one_anchor_carrying_that_exact_uri() {
    let key = "e\u{0}id7\u{0}x";
    let element = paint(
        vec![
            run("see "),
            linked("Foo.txt", "https://ex.test/foo?a=1&b=2", key),
        ],
        None,
    );
    let anchors = anchors_of(&element);
    assert_eq!(anchors.len(), 1);
    let anchor = &anchors[0];
    assert_eq!(
        anchor.attribute("href").as_deref(),
        Some("https://ex.test/foo?a=1&b=2")
    );
    assert_eq!(anchor.class_name(), TERMINAL_LINK_CLASS);
    assert_eq!(anchor.attribute(LINK_KEY_ATTR).as_deref(), Some(key));
    assert_eq!(anchor.attribute("target").as_deref(), Some("_blank"));
    assert_eq!(
        anchor.attribute("rel").as_deref(),
        Some("noopener noreferrer")
    );
    assert_eq!(
        anchor.attribute("data-hint").as_deref(),
        Some("https://ex.test/foo?a=1&b=2")
    );
    assert_eq!(child_texts(anchor), ["Foo.txt"]);
    assert_eq!(element.text_content(), "see Foo.txt");
}

#[test]
fn two_adjacent_spans_with_different_link_keys_paint_two_anchors() {
    let element = paint(
        vec![
            linked("report", "https://ex.test/one", "b\u{0}0"),
            linked("report", "https://ex.test/two", "b\u{0}1"),
        ],
        None,
    );
    let anchors = anchors_of(&element);
    let hrefs: Vec<_> = anchors
        .iter()
        .map(|anchor| anchor.attribute("href").unwrap())
        .collect();
    assert_eq!(hrefs, ["https://ex.test/one", "https://ex.test/two"]);
    let keys: Vec<_> = anchors
        .iter()
        .map(|anchor| anchor.attribute(LINK_KEY_ATTR).unwrap())
        .collect();
    assert_eq!(keys, ["b\u{0}0", "b\u{0}1"]);
    assert_eq!(element.text_content(), "reportreport");
}

#[test]
fn adjacent_spans_sharing_a_link_key_paint_one_anchor() {
    let element = paint(
        vec![
            linked("bold", "https://ex.test/x", "b\u{0}2"),
            linked("plain", "https://ex.test/x", "b\u{0}2"),
        ],
        None,
    );
    let anchors = anchors_of(&element);
    assert_eq!(anchors.len(), 1);
    assert_eq!(child_texts(&anchors[0]), ["bold", "plain"]);
}

#[test]
fn unlinked_cells_between_two_same_key_runs_end_the_anchor() {
    let element = paint(
        vec![
            linked("a", "https://ex.test/x", "b\u{0}3"),
            run(" gap "),
            linked("b", "https://ex.test/x", "b\u{0}3"),
        ],
        None,
    );
    let tags: Vec<_> = element.children().iter().map(FakeEl::tag).collect();
    assert_eq!(tags, ["a", "span", "a"]);
    assert_eq!(element.text_content(), "a gap b");
}

#[test]
fn a_find_hit_inside_a_link_keeps_both_halves_inside_the_one_anchor() {
    let hits = [FindHit { col: 2, len: 2 }];
    let element = paint(
        vec![linked("abcdef", "https://ex.test/hit", "b\u{0}4")],
        Some(&hits),
    );
    let anchors = anchors_of(&element);
    assert_eq!(anchors.len(), 1);
    assert_eq!(child_texts(&anchors[0]), ["ab", "cd", "ef"]);
    let classes: Vec<_> = anchors[0]
        .children()
        .iter()
        .map(|piece| piece.class_name())
        .collect();
    assert_eq!(classes, ["", "cell-find-hit", ""]);
    assert_eq!(element.text_content(), "abcdef");
}

#[test]
fn a_find_hit_in_a_link_that_starts_mid_row_lands_on_its_grid_columns() {
    let hits = [FindHit { col: 6, len: 2 }];
    let element = paint(
        vec![
            run("see "),
            linked("abcdef", "https://ex.test/hit", "b\u{0}4"),
        ],
        Some(&hits),
    );
    let anchors = anchors_of(&element);
    assert_eq!(child_texts(&anchors[0]), ["ab", "cd", "ef"]);
    let classes: Vec<_> = anchors[0]
        .children()
        .iter()
        .map(|piece| piece.class_name())
        .collect();
    assert_eq!(classes, ["", "cell-find-hit", ""]);
}

#[test]
fn only_http_and_worker_file_targets_paint_anchors() {
    for uri in [
        "javascript:alert(1)",
        "  https://space.invalid",
        "data:text/html,<b>",
        "vbscript:x",
        "vscode://file/a.ts",
        "ssh://host/a.ts",
        "//protocol-relative.invalid/a.ts",
    ] {
        let element = paint(
            vec![run("prefix "), linked("click me", uri, "b\u{0}5")],
            None,
        );
        assert!(anchors_of(&element).is_empty(), "{uri}");
        assert_eq!(element.text_content(), "prefix click me");
    }
    let element = paint(
        vec![linked("source", "file:///tmp/a.ts#L9", "b\u{0}6")],
        None,
    );
    let file = &anchors_of(&element)[0];
    assert_eq!(file.attribute("href"), None);
    assert_eq!(file.attribute("data-kind").as_deref(), Some("file"));
    assert_eq!(
        file.attribute("data-terminal-target").as_deref(),
        Some("file:///tmp/a.ts#L9")
    );
}

#[test]
fn the_row_link_marker_is_present_exactly_when_an_anchor_was_painted() {
    let marker = |spans| paint(spans, None).attribute(ROW_HAS_LINKS_ATTR);
    assert_eq!(
        marker(vec![linked("x", "https://ex.test/x", "b\u{0}8")]).as_deref(),
        Some("1")
    );
    assert_eq!(marker(vec![run("plain text")]), None);
    assert_eq!(marker(Vec::new()), None);
    assert_eq!(
        marker(vec![linked("x", "javascript:alert(1)", "b\u{0}9")]),
        None
    );
}

#[test]
fn the_stamp_is_columns_not_code_units() {
    let cjk = paint(vec![run_of("中中中", 6), run("http")], None);
    assert_eq!(cjk.attribute(ROW_COLUMNS_ATTR).as_deref(), Some("10"));
    assert_eq!(cjk.text_content().encode_utf16().count(), 7);
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let cluster = paint(vec![run_of(family, 2), run("x.ts")], None);
    assert_eq!(cluster.attribute(ROW_COLUMNS_ATTR).as_deref(), Some("6"));
    assert_eq!(cluster.text_content().encode_utf16().count(), 15);
}

#[test]
fn a_blank_row_stamps_zero_columns_and_still_paints_its_line_box() {
    let blank = paint(Vec::new(), None);
    assert_eq!(blank.attribute(ROW_COLUMNS_ATTR).as_deref(), Some("0"));
    assert!(blank.children().is_empty());
    assert_eq!(blank.text_content(), " ");
}

#[test]
fn the_row_hash_sees_link_identity() {
    let hash = |spans| row_hash(&row(spans), None, None);
    let one = || linked("report", "https://ex.test/one", "b\u{0}0");
    assert_ne!(
        hash(vec![one()]),
        hash(vec![linked("report", "https://ex.test/two", "b\u{0}1")])
    );
    assert_ne!(hash(vec![run("report")]), hash(vec![one()]));
    assert_eq!(hash(vec![one()]), hash(vec![one()]));
}
