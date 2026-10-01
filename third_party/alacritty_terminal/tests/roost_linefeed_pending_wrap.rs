//! The guard on P6: a line feed clears a pending wrap. See `ROOST-PATCHES.md`.
//!
//! A row written to its last column arms the wrap, and the next printable cell
//! spends it. An LF that leaves it armed lets that cell wrap into the row BELOW
//! the one the LF moved to, so the cursor ends one row too low and every line
//! after it cascades.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

const COLUMNS: usize = 10;
const ROWS: usize = 5;

struct ColsRows(usize, usize);

impl Dimensions for ColsRows {
    fn total_lines(&self) -> usize {
        self.1
    }

    fn screen_lines(&self) -> usize {
        self.1
    }

    fn columns(&self) -> usize {
        self.0
    }
}

#[test]
fn a_line_feed_after_a_full_width_row_disarms_the_pending_wrap() {
    let mut term = Term::new(Config::default(), &ColsRows(COLUMNS, ROWS), VoidListener);
    let mut parser: Processor = Processor::default();

    parser.advance(&mut term, "a".repeat(COLUMNS).as_bytes());
    // A bare LF: it moves down and keeps the column. A CR would clear the wrap
    // on its own and prove nothing about the LF.
    parser.advance(&mut term, b"\n");
    parser.advance(&mut term, b"x");

    assert_eq!(
        term.grid().cursor.point,
        Point::new(Line(1), Column(COLUMNS - 1)),
        "the cell after the LF lands on the row the LF moved to"
    );
    assert_eq!(term.grid()[Line(1)][Column(COLUMNS - 1)].c, 'x');
    let row_below: String =
        (0..COLUMNS).map(|column| term.grid()[Line(2)][Column(column)].c).collect();
    assert_eq!(row_below.trim(), "", "nothing wrapped into the row below");
}
