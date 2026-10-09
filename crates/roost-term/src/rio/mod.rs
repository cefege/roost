//! `rio-vt` (vendored with patches R1–R6, `third_party/rio_vt`) behind the
//! [`TerminalCore`] trait.
//!
//! The adapter owns one `Crosswords` and the `Processor` that drives it: the
//! crate is an emulation core with no byte-input method, so an embedder
//! supplies the parser. Scrollback is addressed newest-first here, matching
//! the trait; rio's `Line(-1)` is the newest history line, so
//! `scrollback_line` converts once, in the one place that does.
//!
//! The listener ([`listener::RioListener`]) queues replies, clipboard stores,
//! bells, dropped CSI and decoded images for the core to drain. A replay
//! `write` empties the first four. Synchronized updates (`CSI ? 2026 h`) are
//! parsed through: rio's processor buffers the block, and `parse` ends every
//! buffered update at once, because the worker withholds FRAMES during one
//! (`crates/roost-worker/src/session/sync_output.rs`) and a probe inside the
//! block must be answered in stream order.

pub(crate) mod cell;
pub(crate) mod images;
pub(crate) mod listener;
mod prompt_marks;
mod prompt_marks_apply;
mod signal_queue;

use std::collections::VecDeque;

use rio_vt::ansi::CursorShape;
use rio_vt::crosswords::grid::Dimensions;
use rio_vt::crosswords::pos::{Column, Line};
use rio_vt::crosswords::square::Wide;
use rio_vt::crosswords::{Crosswords, CrosswordsSize, Mode, TermDamage};
use rio_vt::event::WindowId;
use rio_vt::performer::handler::Processor;

use crate::core::{CommandEvent, CursorState, TerminalCore};
use crate::unhandled::UnhandledSequenceRing;
use cell::LinkScope;
use listener::RioListener;
pub use listener::{NOMINAL_CELL_HEIGHT_PX, NOMINAL_CELL_WIDTH_PX};

/// The scrollback capacity Roost runs every terminal with.
pub const SCROLLBACK_LINES: usize = 10_000;

/// A terminal backed by `rio-vt`.
pub struct RioCore {
    term: Crosswords<RioListener>,
    processor: Processor,
    /// The queues the listener the term owns writes into.
    queues: RioListener,
    /// The dropped-CSI ring as of the last parse, lent by
    /// `unhandled_sequences`.
    unhandled: UnhandledSequenceRing,
    /// Per-core link identity; see `cell` for why rio's own ids are not usable
    /// directly.
    links: LinkScope,
    /// Which viewport rows changed since the last `clear_dirty`. Rio's damage
    /// is read once per parse and kept here, because `damage()` has side
    /// effects (it damages the old cursor line) and must not be asked per row.
    dirty: Vec<u16>,
    /// Carries an OSC 133 mark split across PTY chunks to the next one.
    prompt_marks: prompt_marks::PromptMarkScanner,
    /// Whether a command ran since the last prompt, and how it ended.
    command_lifecycle: prompt_marks_apply::CommandLifecycle,
    /// Live shell command lifecycle events not yet taken by the worker.
    command_events: VecDeque<CommandEvent>,
    images: images::ImageStore,
    /// The shell's OSC 1337 user variables as last observed.
    user_vars: signal_queue::UserVarsWatch,
}

impl RioCore {
    /// A core of `cols` by `rows` with Roost's scrollback capacity.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_history(cols, rows, SCROLLBACK_LINES)
    }

    /// A core with an explicit scrollback capacity, for a test that needs a
    /// small ring and reaches saturation in a readable number of lines.
    pub fn with_history(cols: u16, rows: u16, scrolling_history: usize) -> Self {
        let queues = RioListener::default();
        queues.text_area.set(cols, rows);
        let mut term = Crosswords::new(
            grid_size(cols, rows),
            CursorShape::Block,
            queues.clone(),
            WindowId::from(0),
            0,
            scrolling_history,
        );
        // Roost's wire and every client lay cells out by wcwidth; mode 2027
        // clustering would put a multi-codepoint emoji in one narrow cell.
        term.set_grapheme_clustering(false);
        Self {
            term,
            processor: Processor::default(),
            queues,
            unhandled: UnhandledSequenceRing::default(),
            links: LinkScope::new(),
            prompt_marks: prompt_marks::PromptMarkScanner::default(),
            command_lifecycle: prompt_marks_apply::CommandLifecycle::default(),
            command_events: VecDeque::new(),
            dirty: Vec::new(),
            images: images::ImageStore::default(),
            user_vars: signal_queue::UserVarsWatch::default(),
        }
    }
}

/// A core is a terminal's whole scrollback; rendering it into a log would be
/// its own incident. What an operator needs to see is the shape and the
/// counters, and that is what this prints.
impl std::fmt::Debug for RioCore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RioCore")
            .field("cols", &self.cols())
            .field("rows", &self.rows())
            .field("scrollback_retained", &self.scrollback_count())
            .field("lines_evicted", &self.term.lines_evicted())
            .field("dirty_rows", &self.dirty.len())
            .finish_non_exhaustive()
    }
}

impl TerminalCore for RioCore {
    fn write(&mut self, bytes: &[u8]) {
        self.parse(bytes);
        self.queues.replies.discard();
        self.queues.clipboard.discard();
        self.queues.bells.discard();
        self.queues.signals.discard();
        self.command_events.clear();
    }

    fn write_raw(&mut self, bytes: &[u8]) {
        self.parse(bytes);
    }

    fn get_response(&mut self) -> Option<String> {
        self.queues.replies.pop()
    }

    fn take_clipboard_writes(&mut self) -> Vec<String> {
        self.queues.clipboard.take()
    }

    fn take_command_events(&mut self) -> Vec<CommandEvent> {
        self.command_events.drain(..).collect()
    }

    fn take_bell_events(&mut self) -> u32 {
        self.queues.bells.take()
    }

    fn take_progress(&mut self) -> Option<crate::signals::TerminalProgress> {
        self.queues.signals.take_progress()
    }

    fn take_desktop_notifications(&mut self) -> Vec<crate::signals::TerminalNotification> {
        self.queues.signals.take_notifications()
    }

    fn user_vars(&self) -> Vec<crate::signals::TerminalUserVar> {
        self.user_vars.current()
    }

    fn take_user_vars_changed(&mut self) -> bool {
        self.user_vars.take_changed()
    }

    fn unhandled_sequences(&self) -> &UnhandledSequenceRing {
        &self.unhandled
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(grid_size(cols, rows));
        self.queues.text_area.set(cols, rows);
        self.term.mark_fully_damaged();
        // A resize reflows and moves every row, so nothing is clean after it.
        self.snapshot_damage();
    }

    fn cols(&self) -> u16 {
        self.term.grid.columns() as u16
    }

    fn rows(&self) -> u16 {
        self.term.grid.screen_lines() as u16
    }

    fn scrollback_count(&self) -> usize {
        self.term.grid.history_size()
    }

    fn discarded_line_count(&self) -> Option<u64> {
        Some(self.term.lines_evicted())
    }

    fn viewport_cell(&self, row: u16, col: u16) -> crate::core::CellData {
        cell::cell_data(
            &self.term,
            Line(i32::from(row)),
            Column(usize::from(col)),
            &self.links,
        )
    }

    fn scrollback_line_len(&self, offset: usize) -> usize {
        self.term.grid[scrollback_line(offset)].len()
    }

    fn scrollback_cell(&self, offset: usize, col: u16) -> crate::core::CellData {
        cell::cell_data(
            &self.term,
            scrollback_line(offset),
            Column(usize::from(col)),
            &self.links,
        )
    }

    fn viewport_row_mark(&self, row: u16) -> u8 {
        self.term.grid[Line(i32::from(row))].roost_mark
    }

    fn scrollback_row_mark(&self, offset: usize) -> u8 {
        self.term.grid[scrollback_line(offset)].roost_mark
    }

    fn is_dirty_row(&self, row: u16) -> bool {
        self.dirty.contains(&row)
    }

    fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    fn cursor(&self) -> CursorState {
        let mut pos = self.term.grid.cursor.pos;
        // A cursor parked on a wide glyph's second column is on a cell that is
        // not there; the lead is what the operator sees the cursor inside.
        if pos.col.0 > 0 && self.term.grid[pos.row][pos.col].wide() == Wide::Spacer {
            pos.col.0 -= 1;
        }
        let mode = self.term.mode();
        CursorState {
            row: u16::try_from(pos.row.0).unwrap_or_default(),
            col: u16::try_from(pos.col.0).unwrap_or_default(),
            visible: mode.contains(Mode::SHOW_CURSOR) && !mode.contains(Mode::VI),
        }
    }

    fn using_alt_screen(&self) -> bool {
        self.term.mode().contains(Mode::ALT_SCREEN)
    }

    fn cursor_keys_app(&self) -> bool {
        self.term.mode().contains(Mode::APP_CURSOR)
    }

    fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(Mode::BRACKETED_PASTE)
    }

    fn mouse_tracking(&self) -> roost_protocol::cell::MouseTracking {
        cell::mouse_tracking(self.term.mode())
    }

    fn mouse_sgr(&self) -> bool {
        self.term.mode().contains(Mode::SGR_MOUSE)
    }

    fn focus_events(&self) -> bool {
        self.term.mode().contains(Mode::FOCUS_IN_OUT)
    }

    fn kitty_keyboard_flags(&self) -> u8 {
        self.term.keyboard_mode().bits() & 0x1f
    }
    fn image_placements(&self) -> Vec<crate::core::CoreImagePlacement> {
        self.images.placements(&self.term)
    }

    fn image_png(&mut self, image_key: u64) -> Option<std::sync::Arc<[u8]>> {
        self.images.png(image_key)
    }

    fn take_image_changes(&mut self) -> bool {
        let graphics_dirty = self.term.graphics.kitty_graphics_dirty;
        self.term.graphics.kitty_graphics_dirty = false;
        let current = self.images.placements(&self.term);
        let placements_changed = self
            .images
            .last_placements
            .as_ref()
            .is_some_and(|previous| previous != &current);
        self.images.last_placements = Some(current);
        self.images.take_changed() || graphics_dirty || placements_changed
    }
}

impl RioCore {
    /// The parse and the damage read one PTY chunk costs.
    fn parse(&mut self, bytes: &[u8]) {
        let marks = self.prompt_marks.scan(bytes);
        let mut start = 0;
        for marker in marks {
            self.advance(&bytes[start..marker.end]);
            self.apply_prompt_mark(marker.mark);
            start = marker.end;
        }
        self.advance(&bytes[start..]);
        self.queues.unhandled.refresh(&mut self.unhandled);
        for graphics in self.queues.graphics.take() {
            self.images.ingest(graphics, &self.term);
        }
        self.user_vars.observe(&self.term.user_vars);
        self.snapshot_damage();
    }

    /// Parse `bytes`, ending any synchronized update they opened.
    fn advance(&mut self, bytes: &[u8]) {
        self.processor.advance(&mut self.term, bytes);
        if self.processor.sync_bytes_count() > 0 {
            self.processor.stop_sync(&mut self.term);
        }
    }

    /// Read rio's damage once and keep the line numbers.
    fn snapshot_damage(&mut self) {
        let rows = self.rows();
        let damaged: Vec<u16> = match self.term.damage() {
            TermDamage::Full => (0..rows).collect(),
            TermDamage::Partial(lines) => lines
                .filter(|line| line.damaged)
                .filter_map(|line| u16::try_from(line.line).ok())
                .filter(|row| *row < rows)
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

/// The grid line for a newest-first scrollback offset: rio's history runs
/// newest at `Line(-1)` and oldest at `Line(-history_size())`.
fn scrollback_line(offset: usize) -> Line {
    Line(
        -(i32::try_from(offset)
            .unwrap_or(i32::MAX - 1)
            .saturating_add(1)),
    )
}

/// A size carrying the nominal pixel geometry, so sixel and iTerm2 images —
/// which rio drops when the cell size is zero — are sized in cells.
fn grid_size(cols: u16, rows: u16) -> CrosswordsSize {
    CrosswordsSize::new_with_dimensions(
        usize::from(cols),
        usize::from(rows),
        u32::from(cols) * NOMINAL_CELL_WIDTH_PX,
        u32::from(rows) * NOMINAL_CELL_HEIGHT_PX,
        NOMINAL_CELL_WIDTH_PX,
        NOMINAL_CELL_HEIGHT_PX,
    )
}
