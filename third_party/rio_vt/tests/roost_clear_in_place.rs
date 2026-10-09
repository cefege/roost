//! The guard on R3: `CSI 2J` clears the viewport in place instead of
//! scrolling it into history. See `ROOST-PATCHES.md`.

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
fn erase_display_leaves_history_untouched() {
    let mut term = term(10, 4, 100);
    let mut parser = Processor::default();

    parser.advance(&mut term, b"top\r\nmiddle\r\nbottom");
    let before = term.history_size();
    parser.advance(&mut term, b"\x1b[2J");

    assert_eq!(
        term.history_size(),
        before,
        "no line was pushed into history"
    );
    assert_eq!(term.lines_evicted(), 0);
    for line in 0..4 {
        assert_eq!(row_text(&term, line, 10).trim(), "", "row {line} is blank");
    }
}
