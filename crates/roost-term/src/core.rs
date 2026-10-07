//! The capability surface a terminal core must offer, and the one cell shape
//! every implementation produces.
//!
//! This trait is the whole contract between a terminal emulator and its two
//! readers: the frame emitter, and the worker's query-reply lane and
//! unhandled-sequence diagnostics. It exists so the emulator can be replaced — it was a patched
//! WebAssembly build of a Zig library and is now `alacritty_terminal` — without
//! the emitter, the wire format, or a single test noticing. The method names
//! are the v2 WebAssembly ABI's, deliberately: that ABI is what the emitter's
//! behaviour, its conformance vectors and its incident history are written
//! against, so keeping the vocabulary means a port can be checked line by line
//! against the thing it replaces.
//!
//! Nothing here is async and nothing here owns a clock. A core is fed bytes
//! and asked what it now holds.

use crate::error::{TerminalCoreError, TerminalCoreResult};
use crate::unhandled::UnhandledSequenceRing;

/// A terminal's cursor, as a position in the viewport plus whether it is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorState {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
}

/// One cell, as the emitter reads it.
///
/// A cell is a code point plus, optionally, the zero-width characters that
/// combine onto it, plus its style. `width` is the reason this cannot be a
/// `char`: a double-width glyph is a width-2 lead cell followed by a width-0
/// continuation cell, and an orphan continuation is a cell of its own that
/// still occupies a column. The emitter folds continuations into their lead;
/// everything that decides how is here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellData {
    /// The cell's code point. A NUL is the wide-character continuation marker,
    /// never a printable character.
    pub character: u32,
    /// Zero-width characters combining onto `character`, as code points. A
    /// cluster is one cell and may not be sliced.
    pub combining: Option<Vec<u32>>,
    /// Terminal columns this cell occupies: 1 narrow, 2 wide lead, 0 wide
    /// continuation.
    pub width: u8,
    /// Palette index, or [`crate::DEFAULT_COLOR`] for the terminal default.
    pub fg: u16,
    pub bg: u16,
    /// The `CELL_*` bitfield; see `roost_protocol::cell`.
    pub flags: u16,
    /// 0xRRGGBB, when the colour is not a palette entry.
    pub fg_rgb: Option<u32>,
    pub bg_rgb: Option<u32>,
    /// URI associated with the OSC 8 link run.
    pub link_uri: Option<String>,
    /// Identity of the link run this cell belongs to.
    pub link_key: Option<String>,
    /// OSC 133 semantic marks stored on the emulator cell.
    pub semantic_mark: u8,
}

impl Default for CellData {
    fn default() -> Self {
        Self {
            character: ' ' as u32,
            combining: None,
            width: 1,
            fg: crate::DEFAULT_COLOR,
            bg: crate::DEFAULT_COLOR,
            flags: 0,
            fg_rgb: None,
            bg_rgb: None,
            link_uri: None,
            link_key: None,
            semantic_mark: 0,
        }
    }
}
/// A live command lifecycle marker parsed from shell integration output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandEvent {
    Started,
    Finished { exit_code: i32 },
}

/// A terminal emulator the emitter can read.
///
/// The scrollback addressing is **newest-first**: offset 0 is the line just
/// above the viewport and offset `scrollback_count() - 1` the oldest retained.
/// That is the v2 ABI's convention and it is deliberately kept, because the
/// emitter converts it to Roost's oldest-first monotonic index in exactly one
/// place. An implementation reads a line by its offset, never by an absolute
/// index: the absolute index is a property of the ring's eviction history, and
/// only the emitter knows it.
pub trait TerminalCore {
    /// Feed PTY output. Infallible: an emulator that cannot parse a byte
    /// renders nothing for it, which is the emulator's decision and not an
    /// error the caller could act on.
    ///
    /// Any reply or clipboard write the bytes provoke is DISCARDED, along with
    /// anything still queued: this is the replay and rebuild entry, and a probe
    /// answered from history would be a reply to a question nobody is waiting
    /// on — landing on the application's stdin ahead of the next live answer,
    /// which is how a cursor report ends up answering a device-attributes
    /// query. A clipboard write replayed from history would overwrite the
    /// operator's clipboard with text they copied long ago. v2 drained and
    /// dropped the queue after every such write; here a plain write cannot
    /// leave one behind.
    fn write(&mut self, bytes: &[u8]);

    /// Feed PTY output and KEEP the replies and clipboard writes it provokes
    /// for [`TerminalCore::get_response`] and
    /// [`TerminalCore::take_clipboard_writes`]. v2's `writeRaw`: the live
    /// ingest is its only caller, and it drains both after every call.
    fn write_raw(&mut self, bytes: &[u8]);

    /// Pop the oldest reply the core owes the application, `None` when none is
    /// queued. v2's `getResponse`: ONE per call, in the order the core
    /// produced them, so a caller drains until `None`.
    fn get_response(&mut self) -> Option<String>;

    /// Take the OSC 52 clipboard stores the live bytes asked for, oldest
    /// first, already decoded. Write-only: a program can never read the
    /// operator's clipboard through this core.
    fn take_clipboard_writes(&mut self) -> Vec<String>;
    /// Number of bell events parsed from live PTY output since the last take.
    fn take_bell_events(&mut self) -> u32;
    /// Take command lifecycle markers parsed from the live PTY stream, oldest first.
    fn take_command_events(&mut self) -> Vec<CommandEvent>;

    /// The CSI sequences this core's dispatcher dropped, as a never-cleared
    /// ring. A partial detector by construction: it sees unrecognised CSI, not
    /// an OSC or a mode number the emulator accepts and ignores.
    fn unhandled_sequences(&self) -> &UnhandledSequenceRing;

    /// Resize the viewport. A core that reflows its history reports the change
    /// through the discarded count on the next read.
    fn resize(&mut self, cols: u16, rows: u16);

    fn cols(&self) -> u16;
    fn rows(&self) -> u16;

    /// How many history lines are retained right now.
    fn scrollback_count(&self) -> usize;

    /// How many history lines the ring has evicted since this core was
    /// created.
    ///
    /// `None` means this core cannot answer, and that is not a degraded mode —
    /// it is a refusal. Roost addresses history by a monotonic index, and
    /// without this count every absolute index silently re-aliases the moment
    /// the ring saturates. `scrollback_origin` turns `None` into an error
    /// rather than guessing.
    fn discarded_line_count(&self) -> Option<u64>;

    /// One viewport row's cell at `col`. `row` is 0-based from the top.
    fn viewport_cell(&self, row: u16, col: u16) -> CellData;

    /// The stored width of a retained line, which can exceed the current
    /// viewport width: a history line keeps the width it was written at.
    fn scrollback_line_len(&self, offset: usize) -> usize;

    /// One retained line's cell at `col`, addressed newest-first.
    fn scrollback_cell(&self, offset: usize, col: u16) -> CellData;

    /// Whether a viewport row changed since the last [`TerminalCore::clear_dirty`].
    fn is_dirty_row(&self, row: u16) -> bool;

    /// Reset the dirty set. The caller MUST call this after consuming a frame,
    /// or the next delta repeats every row it already sent.
    fn clear_dirty(&mut self);

    fn cursor(&self) -> CursorState;
    fn using_alt_screen(&self) -> bool;
    /// `DECCKM` — the application cursor-key mode.
    fn cursor_keys_app(&self) -> bool;
    /// `DECSET 2004`.
    fn bracketed_paste(&self) -> bool;
    /// `DECSET 1000` / `1002`, with `1003` and the legacy mode 9 folded away by
    /// the core, exactly as the v2 core folded them.
    fn mouse_tracking(&self) -> roost_protocol::cell::MouseTracking;
    /// `DECSET 1006`.
    fn mouse_sgr(&self) -> bool;
    /// `DECSET 1004`.
    fn focus_events(&self) -> bool;

    /// Active kitty keyboard protocol flags, as a five-bit mask.
    fn kitty_keyboard_flags(&self) -> u8 {
        0
    }

    /// The eviction origin every absolute history index is measured from.
    ///
    /// This is a read of the core's own counter, not an inference from
    /// retained history: the v2 implementation re-identified previously-newest
    /// lines by content hash, which cost about 1200 reads per emit near the
    /// cap, went blind past a 256-line scan window, and could alias two
    /// identical tails.
    fn scrollback_origin(&self, base: u64) -> TerminalCoreResult<u64> {
        match self.discarded_line_count() {
            Some(discarded) => Ok(base.saturating_add(discarded)),
            None => Err(TerminalCoreError::NoDiscardedLineCount),
        }
    }
}
