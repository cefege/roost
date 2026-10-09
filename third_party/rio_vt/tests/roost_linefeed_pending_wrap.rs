//! The guard on R1: a line feed clears a pending wrap. See `ROOST-PATCHES.md`.
//!
//! A row written to its last column arms the wrap and the next printable
//! spends it. An LF that leaves it armed lets that cell wrap into the row
//! below the one the LF moved to, so an in-place rewrite walks downward.

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

const COLUMNS: usize = 10;

#[test]
fn a_line_feed_after_a_full_width_row_disarms_the_pending_wrap() {
    let mut term = term(COLUMNS, 5, 100);
    let mut parser = Processor::default();

    parser.advance(&mut term, "a".repeat(COLUMNS).as_bytes());
    // A bare LF keeps the column; a CR would clear the wrap on its own.
    parser.advance(&mut term, b"\n");
    parser.advance(&mut term, b"x");

    assert_eq!(
        term.grid.cursor.pos.row,
        Line(1),
        "the cell lands on the LF's row"
    );
    assert_eq!(term.grid.cursor.pos.col, Column(COLUMNS - 1));
    assert_eq!(term.grid[Line(1)][Column(COLUMNS - 1)].c(), 'x');
    assert_eq!(
        row_text(&term, 2, COLUMNS).trim(),
        "",
        "nothing wrapped below"
    );
}
