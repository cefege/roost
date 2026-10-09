//! The guard on R5: `Row::roost_mark` travels with its row through reflow
//! and clears with it. See `ROOST-PATCHES.md`.

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
fn a_row_mark_survives_reflow_and_clears_on_erase() {
    let mut term = term(8, 3, 100);
    let mut parser = Processor::default();

    parser.advance(&mut term, b"$ ls");
    term.grid[Line(0)].roost_mark = 1;
    assert_eq!(row_text(&term, 0, 8).trim(), "$ ls");

    term.resize(CrosswordsSize::new(4, 3));
    assert_eq!(
        term.grid[Line(0)].roost_mark,
        1,
        "kept on a narrower reflow"
    );
    term.resize(CrosswordsSize::new(12, 3));
    assert_eq!(term.grid[Line(0)].roost_mark, 1, "kept on a wider reflow");

    parser.advance(&mut term, b"x");
    assert_eq!(term.grid[Line(0)].roost_mark, 1, "kept across a write");

    parser.advance(&mut term, b"\x1b[2J");
    assert_eq!(term.grid[Line(0)].roost_mark, 0, "cleared by an erase");
}

#[test]
fn a_row_mark_travels_into_history() {
    let mut term = term(8, 2, 100);
    let mut parser = Processor::default();

    term.grid[Line(0)].roost_mark = 4;
    parser.advance(&mut term, b"a\r\nb\r\nc");
    assert_eq!(term.grid[Line(-1)].roost_mark, 4);
    assert_eq!(
        term.grid[Line(1)].roost_mark,
        0,
        "a recycled row starts clear"
    );
}
