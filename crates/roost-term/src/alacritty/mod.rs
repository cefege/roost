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
//! The event listener is [`replies::ReplyListener`]: `PtyWrite` — alacritty
//! answering a probe — is queued for the query-reply lane, an OSC 52 store is
//! queued for the browser's clipboard, a BEL is counted for the worker's
//! one-shot bell event, and every other event (title) is dropped, because the
//! PTY reader behind it belongs to the worker, not to a core that only
//! renders. All three queues are emptied by a replay `write`. The
//! processor parses THROUGH synchronized updates ([`replies::ParseThrough`]),
//! and a shadow parser ([`csi_shadow`]) records the CSI sequences `vte` drops.

pub(crate) mod cell;
mod csi_shadow;
mod prompt_marks;
mod prompt_marks_apply;
mod replies;

use std::collections::VecDeque;

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::Processor;

use crate::core::{CommandEvent, CursorState, TerminalCore};
use crate::unhandled::UnhandledSequenceRing;
use cell::LinkScope;
use csi_shadow::CsiShadow;
use replies::{BellQueue, ClipboardQueue, ParseThrough, ReplyListener, ReplyQueue};

/// The scrollback capacity Roost runs every terminal with.
pub const SCROLLBACK_LINES: usize = 10_000;

/// A terminal backed by `alacritty_terminal`.
///
pub struct AlacrittyCore {
    term: Term<ReplyListener>,
    processor: Processor<ParseThrough>,
    /// What the term has answered and nobody has popped, shared with the
    /// listener the term owns.
    replies: ReplyQueue,
    /// OSC 52 stores parsed live and not yet taken, shared the same way.
    clipboard_writes: ClipboardQueue,
    /// The second parse that observes dropped CSI; one per core, for its life.
    csi_shadow: CsiShadow,
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
    /// Carries an OSC 133 mark split across PTY chunks to the next one.
    prompt_marks: prompt_marks::PromptMarkScanner,
    /// Whether a command ran since the last prompt, and how it ended.
    command_lifecycle: prompt_marks_apply::CommandLifecycle,
    /// Live shell command lifecycle events not yet taken by the worker.
    command_events: VecDeque<CommandEvent>,
    /// Live bell events not yet taken by the worker.
    bells: BellQueue,
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
        let replies = ReplyQueue::default();
        let clipboard_writes = ClipboardQueue::default();
        let bells = BellQueue::default();
        let term = Term::new(
            config,
            &GridSize { cols, rows },
            ReplyListener::new(replies.clone(), clipboard_writes.clone(), bells.clone()),
        );
        Self {
            term,
            processor: Processor::new(),
            replies,
            clipboard_writes,
            csi_shadow: CsiShadow::default(),
            links: LinkScope::new(),
            prompt_marks: prompt_marks::PromptMarkScanner::default(),
            command_lifecycle: prompt_marks_apply::CommandLifecycle::default(),
            command_events: VecDeque::new(),
            bells,
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
        self.parse(bytes);
        self.replies.discard();
        self.clipboard_writes.discard();
        self.bells.discard();
        self.command_events.clear();
    }

    fn write_raw(&mut self, bytes: &[u8]) {
        self.parse(bytes);
    }

    fn get_response(&mut self) -> Option<String> {
        self.replies.pop()
    }

    fn take_clipboard_writes(&mut self) -> Vec<String> {
        self.clipboard_writes.take()
    }

    fn take_command_events(&mut self) -> Vec<CommandEvent> {
        self.command_events.drain(..).collect()
    }

    fn take_bell_events(&mut self) -> u32 {
        self.bells.take()
    }

    fn unhandled_sequences(&self) -> &UnhandledSequenceRing {
        self.csi_shadow.ring()
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
    /// Both parses and the damage read one PTY chunk costs.
    fn parse(&mut self, bytes: &[u8]) {
        let marks = self.prompt_marks.scan(bytes);
        if marks.is_empty() {
            self.processor.advance(&mut self.term, bytes);
            self.csi_shadow.advance(bytes);
        } else {
            let mut start = 0;
            for marker in marks {
                let chunk = &bytes[start..marker.end];
                self.processor.advance(&mut self.term, chunk);
                self.csi_shadow.advance(chunk);
                self.apply_prompt_mark(marker.mark);
                start = marker.end;
            }
            let remainder = &bytes[start..];
            self.processor.advance(&mut self.term, remainder);
            self.csi_shadow.advance(remainder);
        }
        self.snapshot_damage();
    }

    /// The grid line for a newest-first scrollback offset.
    ///
    /// Alacritty's scrollback runs newest at `Line(-1)` and oldest at
    /// `Line(-history_size())`, so the trait's newest-first offset is simply
    /// the negation with no dependence on how much is retained.
    fn scrollback_line(&self, offset: usize) -> Line {
        Line(-((offset as i32).saturating_add(1)))
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
