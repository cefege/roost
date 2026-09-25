//! The vendored patch: a count of the history lines a grid has scrolled off
//! the top. See `ROOST-PATCHES.md` for why Roost needs it and why it cannot
//! be derived from outside the crate.
//!
//! These cases are the guard on the patch itself. A counter on the wrong site
//! passes a "did it grow" check and still fails the emitter, so each case
//! pins the exact overflow at the cap boundary rather than merely trending
//! upward.
//!
//! The expectations are derived from the terminal rather than hand-counted.
//! A line feed only scrolls once the cursor reaches the last row, so "N
//! newlines scroll N lines" is false until the screen is full, and a literal
//! constant in this file would encode an off-by-one that says nothing about
//! the counter.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

const VIEWPORT_ROWS: usize = 24;

struct ColsRows(usize, usize);

impl Dimensions for ColsRows {
    fn total_lines(&self) -> usize {
        self.0
    }

    fn screen_lines(&self) -> usize {
        self.1
    }

    fn columns(&self) -> usize {
        self.0
    }
}

type TestTerm = Term<VoidListener>;

/// A terminal with `history` scrollback lines, its processor, and its cursor
/// parked at the bottom row so that every later line feed scrolls.
///
/// Parking a cursor that already starts on the last row pushes one line into
/// history on the way down, so the fixture reports the baseline it reached and
/// every expectation is written against that rather than against a literal.
fn terminal(history: usize) -> (TestTerm, Processor, usize) {
    let mut term = Term::new(
        Config { scrolling_history: history, ..Config::default() },
        &ColsRows(80, VIEWPORT_ROWS),
        VoidListener,
    );
    let mut parser = Processor::default();
    scroll(&mut term, &mut parser, VIEWPORT_ROWS + 1);
    let baseline = term.history_size();
    (term, parser, baseline)
}

fn scroll(term: &mut TestTerm, parser: &mut Processor, lines: usize) {
    for _ in 0..lines {
        parser.advance(term, b"\n");
    }
}

/// Scroll until the history is exactly `cap` lines, and report how many line
/// feeds that took.
fn scroll_until_history_is_full(
    term: &mut TestTerm,
    parser: &mut Processor,
    cap: usize,
) -> usize {
    let mut scrolled = 0;
    while term.history_size() < cap {
        scroll(term, parser, 1);
        scrolled += 1;
    }
    scrolled
}

/// The monotonic total a client addresses history against: the lines evicted
/// plus the lines still held.
fn monotonic_total(term: &TestTerm) -> u64 {
    term.discarded_line_count() + term.history_size() as u64
}

#[test]
fn a_terminal_below_its_cap_discards_nothing() {
    let (mut term, mut parser, baseline) = terminal(1_000);
    let before = monotonic_total(&term);
    scroll(&mut term, &mut parser, 500);
    assert_eq!(term.discarded_line_count(), 0, "nothing was evicted");
    assert_eq!(
        term.history_size(),
        baseline + 500,
        "every scrolled line is still held"
    );
    assert_eq!(
        monotonic_total(&term) - before,
        500,
        "the total advanced by exactly the lines scrolled"
    );
}

#[test]
fn a_history_filled_exactly_to_the_cap_discards_nothing() {
    let (mut term, mut parser, _baseline) = terminal(100);
    scroll_until_history_is_full(&mut term, &mut parser, 100);
    assert_eq!(
        term.discarded_line_count(),
        0,
        "a ring that reached its cap but did not pass it lost nothing"
    );
    assert_eq!(term.history_size(), 100, "the cap is exactly full");
}

#[test]
fn one_line_past_the_cap_discards_exactly_one() {
    let (mut term, mut parser, _baseline) = terminal(100);
    scroll_until_history_is_full(&mut term, &mut parser, 100);
    let total = monotonic_total(&term);
    scroll(&mut term, &mut parser, 1);
    assert_eq!(
        term.discarded_line_count(),
        1,
        "the boundary case: the count is the overflow, not the lines scrolled"
    );
    assert_eq!(term.history_size(), 100, "retained history stays at the cap");
    assert_eq!(
        monotonic_total(&term),
        total + 1,
        "the total advanced even though the window did not grow"
    );
}

#[test]
fn scrolling_far_past_the_cap_discards_exactly_the_overflow() {
    let (mut term, mut parser, _baseline) = terminal(100);
    scroll_until_history_is_full(&mut term, &mut parser, 100);
    let at_the_cap = monotonic_total(&term);
    let extra = 1_250;
    scroll(&mut term, &mut parser, extra);
    assert_eq!(
        term.discarded_line_count(),
        extra as u64,
        "every line past the cap is one the terminal can never show again"
    );
    assert_eq!(term.history_size(), 100);
    assert_eq!(monotonic_total(&term), at_the_cap + extra as u64);
}

#[test]
fn the_retained_window_is_contiguous_and_moves_by_one() {
    let (mut term, mut parser, _baseline) = terminal(50);
    scroll_until_history_is_full(&mut term, &mut parser, 50);
    assert_eq!(term.discarded_line_count(), 0, "filling to the cap evicts nothing");
    // Past the cap, every line is one eviction.
    scroll(&mut term, &mut parser, 30);
    let floor = term.discarded_line_count();
    assert_eq!(floor, 30, "the setup must have evicted something");
    let total = monotonic_total(&term);
    scroll(&mut term, &mut parser, 1);
    assert_eq!(
        term.discarded_line_count(),
        floor + 1,
        "one more line moves the floor by exactly one"
    );
    assert_eq!(term.history_size(), 50, "the window keeps its size");
    assert_eq!(
        monotonic_total(&term),
        total + 1,
        "the far edge is the monotonic total a client measures against"
    );
}

#[test]
fn growing_the_viewport_pops_history_back_without_discarding() {
    let (mut term, mut parser, _baseline) = terminal(100);
    scroll_until_history_is_full(&mut term, &mut parser, 100);
    scroll(&mut term, &mut parser, 50);
    let before = term.discarded_line_count();
    assert!(before > 0, "the setup must have evicted something");
    // A taller viewport pulls lines OUT of history into the view. Those lines
    // are still addressable, so the origin must not move — the emitter detects
    // this case by watching its own monotonic total go backwards.
    term.resize(ColsRows(80, 40));
    assert_eq!(
        term.discarded_line_count(),
        before,
        "a viewport grow pops history back rather than losing it"
    );
}

#[test]
fn shrinking_the_viewport_into_a_full_history_discards_exactly_the_growth() {
    let (mut term, mut parser, _baseline) = terminal(100);
    scroll_until_history_is_full(&mut term, &mut parser, 100);
    let before = term.discarded_line_count();
    let shrink = 14;
    term.resize(ColsRows(80, VIEWPORT_ROWS - shrink));
    assert_eq!(
        term.discarded_line_count(),
        before + shrink as u64,
        "lines pushed out of a short viewport have to come out of the top"
    );
}

#[test]
fn clearing_history_does_not_advance_the_count() {
    let (mut term, mut parser, _baseline) = terminal(100);
    scroll_until_history_is_full(&mut term, &mut parser, 100);
    scroll(&mut term, &mut parser, 50);
    let before = term.discarded_line_count();
    assert!(before > 0, "the setup must have evicted something");
    term.grid_mut().clear_history();
    assert_eq!(
        term.discarded_line_count(),
        before,
        "a clear pops lines back to the viewport; it does not lose them"
    );
}
