//! The guard on R2: `CSI M` discards the lines it deletes; only a scroll
//! reaches history. See `ROOST-PATCHES.md`.

use rio_vt::ansi::CursorShape;
use rio_vt::crosswords::pos::{Column, Line};
use rio_vt::crosswords::{Crosswords, CrosswordsSize};
use rio_vt::event::{VoidListener, WindowId};
use rio_vt::performer::handler::Processor;

fn term(columns: usize, rows: usize, history: usize) -> Crosswords<VoidListener> {
    Crosswords::new(
        CrosswordsSize::new(columns, rows),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        history,
    )
}

fn row_text(term: &Crosswords<VoidListener>, line: i32, columns: usize) -> String {
    (0..columns)
        .map(|column| match term.grid[Line(line)][Column(column)].c() {
            '\0' => ' ',
            other => other,
        })
        .collect()
}

#[test]
fn deleting_lines_at_the_top_of_a_full_screen_region_keeps_history_empty() {
    let mut term = term(10, 5, 100);
    let mut parser = Processor::default();

    parser.advance(&mut term, b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
    assert_eq!(term.history_size(), 0);
    parser.advance(&mut term, b"\x1b[H\x1b[2M");

    assert_eq!(term.history_size(), 0, "deleted lines never reach history");
    assert_eq!(term.lines_evicted(), 0, "nor count as evicted");
    assert_eq!(row_text(&term, 0, 10).trim(), "three");
    assert_eq!(row_text(&term, 2, 10).trim(), "five");
    assert_eq!(row_text(&term, 3, 10).trim(), "");
}

#[test]
fn a_line_feed_at_the_bottom_still_scrolls_into_history() {
    let mut term = term(10, 3, 100);
    let mut parser = Processor::default();

    parser.advance(&mut term, b"a\r\nb\r\nc\r\nd");
    assert_eq!(term.history_size(), 1);
    assert_eq!(row_text(&term, -1, 10).trim(), "a");
}
