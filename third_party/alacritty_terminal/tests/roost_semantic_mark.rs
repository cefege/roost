//! The guard on P7: semantic marks survive prompt writes and reflow.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::{Config, Term, test::TermSize};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

#[test]
fn marks_survive_writes_and_reflow_but_clear_on_erase() {
    let mut cell = Cell::default();
    assert_eq!(cell.semantic_mark(), 0);
    assert!(cell.extra.is_none());
    cell.set_semantic_mark(1);
    assert_eq!(cell.semantic_mark(), 1);
    cell.set_semantic_mark(0);
    assert_eq!(cell.semantic_mark(), 0);

    let mut term = Term::new(Config::default(), &TermSize::new(4, 2), VoidListener);
    let point = Point::new(Line(0), Column(0));
    term.grid_mut()[point].set_semantic_mark(1);

    let mut processor = Processor::<StdSyncHandler>::new();
    processor.advance(&mut term, b"p");
    assert_eq!(term.grid()[point].c, 'p');
    assert_eq!(term.grid()[point].semantic_mark(), 1);

    term.grid_mut()[point].set_semantic_mark(2);
    term.resize(TermSize::new(2, 2));
    let reflowed_point = Point::new(Line(0), Column(0));
    assert_eq!(term.grid()[reflowed_point].semantic_mark(), 2);

    processor.advance(&mut term, b"\x1b[2J");
    assert_eq!(term.grid()[reflowed_point].semantic_mark(), 0);
}
