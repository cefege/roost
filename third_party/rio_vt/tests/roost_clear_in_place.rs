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

#[cfg(feature = "graphics")]
#[test]
fn erase_display_and_clear_history_drop_kitty_images() {
    // Kitty placements need a cell size in pixels.
    let mut term = Crosswords::new(
        CrosswordsSize::new_with_dimensions(20, 6, 160, 96, 8, 16),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        100,
    );
    let mut parser = Processor::default();
    // A 1x1 red pixel, placed over 2x1 cells at the cursor.
    parser.advance(&mut term, b"\x1b_Ga=T,f=32,s=1,v=1,c=2,r=1;/wAA/w==\x1b\\");
    assert_eq!(term.graphics.kitty_placements.len(), 1);
    parser.advance(&mut term, b"\x1b[2J");
    assert!(
        term.graphics.kitty_placements.is_empty(),
        "ED 2 erases on-screen images"
    );

    parser.advance(
        &mut term,
        b"\x1b[H\x1b_Ga=T,f=32,s=1,v=1,c=2,r=1;/wAA/w==\x1b\\",
    );
    for _ in 0..10 {
        parser.advance(&mut term, b"\r\n");
    }
    assert_eq!(
        term.graphics.kitty_placements.len(),
        1,
        "scrolled into history"
    );
    parser.advance(&mut term, b"\x1b[3J");
    assert!(
        term.graphics.kitty_placements.is_empty(),
        "ED 3 erases images in history"
    );
}
