//! `alacritty_terminal`'s `Cell` as the emitter's `CellData`.
//!
//! Three conversions carry decisions rather than mechanics.
//!
//! **Width.** Alacritty marks a wide glyph's lead with `WIDE_CHAR` and its
//! second column with `WIDE_CHAR_SPACER`, whose `c` is NUL. A wide glyph that
//! lands at the end of a row leaves `LEADING_WIDE_CHAR_SPACER` behind instead.
//! Those three flags are the whole width model, and a cell with none of them is
//! one column wide.
//!
//! **Colour.** Alacritty's `Color` is `Named`, `Spec(rgb)` or `Indexed(u8)`.
//! Roost's wire carries a palette index for the first 256 entries and a
//! separate 24-bit field otherwise, and `DEFAULT_COLOR` (256) is the terminal
//! default. A `Named` colour has no palette index of its own — it is resolved
//! through the terminal's palette — so it becomes a true-colour value, and a
//! colour the terminal has no override for stays the default.
//!
//! **Link identity.** Alacritty mints an OSC 8 id that has no `id=` in the byte
//! stream from a process-global counter, so two terminals in one process share
//! it and it is not stable across processes. Roost's `link_key` is per-core run
//! identity, so the adapter assigns its own sequential key the first time it
//! sees an alacritty id and reuses it after. That is what makes two adjacent
//! runs of the same link one span, and two different links two spans, without a
//! process-wide counter leaking in.

use std::sync::atomic::{AtomicU32, Ordering};

use alacritty_terminal::Term;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::Color;

use crate::core::CellData;

/// The palette index Roost's wire uses for "whatever the terminal's default is".
const DEFAULT_COLOR: u16 = 256;

/// A per-core prefix, so a `link_key` names a link run within ONE terminal.
///
/// Alacritty mints an OSC 8 id from a process-global counter when the byte
/// stream carries no `id=`, so the raw id is shared by every terminal in the
/// process and says nothing about which terminal a span belongs to. Prefixing
/// it with something unique to this core makes the key per-core without
/// mutating anything to read a cell: the same hyperlink always produces the
/// same key, which is what lets a run of cells fold into one span and two
/// adjacent runs of the same link stay two spans.
static CORE_SERIAL: AtomicU32 = AtomicU32::new(1);

#[derive(Debug, Clone)]
pub(crate) struct LinkScope {
    prefix: String,
}

impl LinkScope {
    pub(crate) fn new() -> Self {
        let serial = CORE_SERIAL.fetch_add(1, Ordering::Relaxed);
        Self {
            prefix: format!("t{serial}"),
        }
    }

    /// The key for one OSC 8 run.
    pub(crate) fn key_for(&self, id: &str) -> String {
        format!("{}-{}", self.prefix, id)
    }
}

/// The terminal columns a cell occupies.
pub(crate) fn cell_width(cell: &Cell) -> u8 {
    if cell.flags.contains(Flags::WIDE_CHAR) {
        2
    } else if cell
        .flags
        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
    {
        0
    } else {
        1
    }
}

/// One cell, in the shape the emitter reads.
pub(crate) fn cell_data(
    term: &Term<impl alacritty_terminal::event::EventListener>,
    cell: &Cell,
    links: &LinkScope,
) -> CellData {
    let (fg, fg_rgb) = palette(term, cell.fg);
    let (bg, bg_rgb) = palette(term, cell.bg);
    let (link_uri, link_key) = match cell.hyperlink() {
        Some(hyperlink) => {
            let uri = hyperlink.uri().to_owned();
            let key = links.key_for(hyperlink.id());
            (Some(uri), Some(key))
        }
        None => (None, None),
    };
    CellData {
        character: cell.c as u32,
        // An EMPTY cluster is not a cluster. Alacritty allocates a cell's
        // `extra` for a hyperlink as well as for combining characters, so
        // `zerowidth()` answers `Some(&[])` on every linked cell; left as
        // `Some([])` it would make each linked cell atomic, and a six-character
        // link would ship as six spans — each its own clickable run.
        combining: cell
            .zerowidth()
            .filter(|chars| !chars.is_empty())
            .map(|chars| chars.iter().map(|c| *c as u32).collect()),
        width: cell_width(cell),
        fg,
        bg,
        flags: roost_flags(cell.flags),
        fg_rgb,
        bg_rgb,
        link_uri,
        link_key,
    }
}

/// A colour as the wire carries it: a palette index, or a true-colour value.
///
/// `Named` has no index — it names a role the terminal's theme resolves — so
/// it becomes true colour when the terminal has an override and the default
/// otherwise. `Spec` is already true colour. `Indexed` is a palette entry.
fn palette(
    term: &Term<impl alacritty_terminal::event::EventListener>,
    color: Color,
) -> (u16, Option<u32>) {
    match color {
        Color::Indexed(index) => (u16::from(index), None),
        Color::Spec(rgb) => (DEFAULT_COLOR, Some(packed(rgb))),
        Color::Named(named) => match term.colors()[named] {
            Some(rgb) => (DEFAULT_COLOR, Some(packed(rgb))),
            None => (DEFAULT_COLOR, None),
        },
    }
}

fn packed(rgb: alacritty_terminal::vte::ansi::Rgb) -> u32 {
    (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b)
}

/// Alacritty's cell flags as the wire's `CELL_*` bitfield.
///
/// The wire has a blink bit and alacritty has no blink flag: a blinking cell is
/// a cursor concern here, not a per-cell SGR attribute, so `CELL_BLINK` is
/// never set by this core. That is a real difference from the v2 core and a
/// client that animated a blinking span will see it stop; it is recorded in the
/// terminal-core conformance family rather than papered over.
fn roost_flags(flags: Flags) -> u16 {
    let mut wire = 0u16;
    if flags.contains(Flags::BOLD) {
        wire |= roost_protocol::cell::CELL_BOLD;
    }
    if flags.contains(Flags::DIM) {
        wire |= roost_protocol::cell::CELL_DIM;
    }
    if flags.contains(Flags::ITALIC) {
        wire |= roost_protocol::cell::CELL_ITALIC;
    }
    // `ALL_UNDERLINES` is a five-bit composite, and `contains` on a composite
    // demands every bit. A cell that is underlined plainly has one of the five,
    // so this has to be `intersects` — with `contains` an ordinary underline
    // never reaches the wire.
    if flags.intersects(Flags::ALL_UNDERLINES) {
        wire |= roost_protocol::cell::CELL_UNDERLINE;
    }
    if flags.contains(Flags::INVERSE) {
        wire |= roost_protocol::cell::CELL_REVERSE;
    }
    if flags.contains(Flags::HIDDEN) {
        wire |= roost_protocol::cell::CELL_INVISIBLE;
    }
    if flags.contains(Flags::STRIKEOUT) {
        wire |= roost_protocol::cell::CELL_STRIKE;
    }
    wire
}

/// Mouse tracking as the wire's closed set: `1000`, `1002`, or none.
///
/// `MOUSE_MOTION` is the `1003` any-motion mode, which the v2 core folded away
/// and so does this one: a client that received it would have to report motion
/// for a terminal the product does not forward it for.
pub(crate) fn mouse_tracking(mode: &TermMode) -> roost_protocol::cell::MouseTracking {
    use roost_protocol::cell::MouseTracking;
    if mode.contains(TermMode::MOUSE_DRAG) {
        MouseTracking::ButtonMotion
    } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
        MouseTracking::PressRelease
    } else {
        MouseTracking::None
    }
}
