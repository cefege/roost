//! `alacritty_terminal` behind the [`TerminalCore`] trait.
//!
//! The adapter owns one `Term` and the `vte::ansi::Processor` that drives it,
//! because the crate is an emulation core rather than a terminal: it has no
//! method that accepts bytes, and an embedder supplies the parser.
//!
//! Scrollback is addressed newest-first here, matching the trait. Alacritty
//! stores scrollback the other way round — `Line(-1)` is the newest line and
//! `Line(-history_size())` the oldest — so `scrollback_line` converts once, in
//! the one place that does.
//!
//! The event listener is `VoidListener`. Alacritty's `Event` has no scroll or
//! history variant, and the events it does carry are the ones a terminal
//! application acts on (title, clipboard, bell); the PTY reader behind them
//! belongs to the worker, not to a core that only renders.

pub(crate) mod cell;

use alacritty_terminal::Term;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::Processor;

use crate::core::{CursorState, TerminalCore};
use cell::LinkScope;

/// The scrollback capacity Roost runs every terminal with.
pub const SCROLLBACK_LINES: usize = 10_000;

/// A terminal backed by `alacritty_terminal`.
///
pub struct AlacrittyCore {
    term: Term<VoidListener>,
    processor: Processor,
    /// Per-core link identity; see `cell` for why alacritty's own ids are not
    /// usable directly.
    links: LinkScope,
    /// Which viewport rows changed since the last `clear_dirty`.
    ///
    /// Alacritty's damage is pull-based: `damage()` takes `&mut self` and has
    /// side effects — it damages the cursor's old and new positions and forces
    /// a full repaint under insert mode. Asking it per row, as the v2 ABI's
    /// `isDirtyRow` invites, would re-derive the set every time and perturb it
    /// while doing so. So the adapter reads it once per write and answers from
    /// this snapshot, which is also one damage read per PTY chunk instead of
    /// one per row.
    dirty: Vec<u16>,
}

impl AlacrittyCore {
    /// A core of `cols` by `rows` with Roost's scrollback capacity.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_history(cols, rows, SCROLLBACK_LINES)
    }

    /// A core with an explicit scrollback capacity, for a test that needs a
    /// small ring and reaches saturation in a readable number of lines.
    pub fn with_history(cols: u16, rows: u16, scrolling_history: usize) -> Self {
        let config = alacritty_terminal::term::Config {
            scrolling_history,
            ..alacritty_terminal::term::Config::default()
        };
        let term = Term::new(config, &GridSize { cols, rows }, VoidListener);
        Self {
            term,
            processor: Processor::default(),
            links: LinkScope::new(),
            dirty: Vec::new(),
        }
    }
}

/// A core is a terminal's whole scrollback; rendering it into a log would be
/// its own incident. What an operator needs to see is the shape and the
/// counters, and that is what this prints.
impl std::fmt::Debug for AlacrittyCore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AlacrittyCore")
            .field("cols", &self.cols())
            .field("rows", &self.rows())
            .field("scrollback_retained", &self.scrollback_count())
            .field("discarded_line_count", &self.term.discarded_line_count())
            .field("dirty_rows", &self.dirty.len())
            .finish_non_exhaustive()
    }
}

impl TerminalCore for AlacrittyCore {
    fn write(&mut self, bytes: &[u8]) {
        self.processor.advance(&mut self.term, bytes);
        self.snapshot_damage();
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(GridSize { cols, rows });
        // A resize reflows and moves every row, so nothing is clean after it.
        self.dirty = (0..rows).collect();
    }

    fn cols(&self) -> u16 {
        self.term.grid().columns() as u16
    }

    fn rows(&self) -> u16 {
        self.term.grid().screen_lines() as u16
    }

    fn scrollback_count(&self) -> usize {
        self.term.grid().history_size()
    }

    fn discarded_line_count(&self) -> Option<u64> {
        // Supplied by the vendored patch; see
        // third_party/alacritty_terminal/ROOST-PATCHES.md. `Some` rather than
        // a bare value so a core built without the patch reports its
        // inability instead of claiming a count of zero.
        Some(self.term.discarded_line_count())
    }

    fn viewport_cell(&self, row: u16, col: u16) -> crate::core::CellData {
        let point = alacritty_terminal::index::Point::new(Line(row as i32), Column(col as usize));
        self.cell_at(point)
    }

    fn scrollback_line_len(&self, offset: usize) -> usize {
        self.term.grid()[self.scrollback_line(offset)].len()
    }

    fn scrollback_cell(&self, offset: usize, col: u16) -> crate::core::CellData {
        let line = self.scrollback_line(offset);
        let point = alacritty_terminal::index::Point::new(line, Column(col as usize));
        self.cell_at(point)
    }

    fn is_dirty_row(&self, row: u16) -> bool {
        self.dirty.contains(&row)
    }

    fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    fn cursor(&self) -> CursorState {
        let point = self.term.grid().cursor.point;
        // A cursor parked on a wide glyph's second column is on a cell that is
        // not there; the lead is what the operator sees the cursor inside.
        let point = self
            .term
            .expand_wide(point, alacritty_terminal::index::Direction::Left);
        CursorState {
            row: u16::try_from(point.line.0).unwrap_or_default(),
            col: u16::try_from(point.column.0).unwrap_or_default(),
            visible: self.term.mode().contains(TermMode::SHOW_CURSOR)
                && !self.term.mode().contains(TermMode::VI),
        }
    }

    fn using_alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    fn cursor_keys_app(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    fn mouse_tracking(&self) -> roost_protocol::cell::MouseTracking {
        cell::mouse_tracking(self.term.mode())
    }

    fn mouse_sgr(&self) -> bool {
        self.term.mode().contains(TermMode::SGR_MOUSE)
    }

    fn focus_events(&self) -> bool {
        self.term.mode().contains(TermMode::FOCUS_IN_OUT)
    }
}

impl AlacrittyCore {
    /// The grid line for a newest-first scrollback offset.
    ///
    /// Alacritty's scrollback runs oldest at `Line(-history_size())` and
    /// newest at `Line(-1)`, so the trait's newest-first offset is the
    /// negation. A caller can only reach this through the two `scrollback_*`
    /// methods, which is the point: the conversion exists in exactly one place.
    fn scrollback_line(&self, offset: usize) -> Line {
        let newest = self.term.grid().history_size();
        let position = newest.saturating_sub(offset + 1);
        Line(-(position as i32) - 1)
    }

    fn cell_at(&self, point: alacritty_terminal::index::Point) -> crate::core::CellData {
        cell::cell_data(&self.term, &self.term.grid()[point], &self.links)
    }

    /// Read alacritty's damage once and keep the line numbers.
    fn snapshot_damage(&mut self) {
        let rows = self.term.grid().screen_lines() as u16;
        let damaged: Vec<u16> = match self.term.damage() {
            alacritty_terminal::term::TermDamage::Full => (0..rows).collect(),
            alacritty_terminal::term::TermDamage::Partial(iterator) => iterator
                .filter(|bounds| bounds.is_damaged())
                .map(|bounds| u16::try_from(bounds.line).unwrap_or(0))
                .collect(),
        };
        self.term.reset_damage();
        for row in damaged {
            if !self.dirty.contains(&row) {
                self.dirty.push(row);
            }
        }
        self.dirty.sort_unstable();
    }
}

/// A size, for the two constructors that take one.
struct GridSize {
    cols: u16,
    rows: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}
