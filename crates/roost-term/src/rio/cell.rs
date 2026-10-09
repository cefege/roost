//! A `rio-vt` `Square` as the emitter's `CellData`.
//!
//! Four conversions carry decisions rather than mechanics.
//!
//! **Width and blanks.** Rio marks a wide glyph's lead `Wide::Wide` and its
//! second column `Wide::Spacer`; a wide glyph that lands at the end of a row
//! leaves `Wide::LeadingSpacer` behind. Spacers are width 0 with a NUL
//! character, the emitter's continuation marker. A never-written narrow cell
//! is NUL in rio too, so a narrow NUL is a space here. A kitty Unicode
//! placeholder (U+10EEEE) is a space as well: its pixels are painted by the
//! image layer, never by text.
//!
//! **Colour.** Rio's `AnsiColor` is `Named`, `Spec(rgb)` or `Indexed(u8)`.
//! Roost's wire carries a palette index for the first 256 entries and a
//! separate 24-bit field otherwise, and `DEFAULT_COLOR` (256) is the terminal
//! default. A `Named` colour the terminal has an OSC 4 override for becomes
//! that true-colour value. Otherwise the sixteen ANSI names are palette
//! indices 0–15, so the viewer's scheme paints them, and every other name
//! (foreground, background, cursor, the dim variants) stays the default. A
//! bg-only cell (rio's compact form for an erased cell with a background)
//! reads its background through `Grid::style_of` like any other cell.
//!
//! **Link identity.** Rio mints an OSC 8 id that has no `id=` in the byte
//! stream from a process-global counter, so two terminals in one process share
//! it. Roost's `link_key` is per-core run identity, so the key is the hyperlink
//! id prefixed with a serial unique to this core.

use std::sync::atomic::{AtomicU32, Ordering};

use rio_vt::config::colors::{AnsiColor, ColorArray};
use rio_vt::crosswords::Crosswords;
use rio_vt::crosswords::Mode;
use rio_vt::crosswords::pos::{Column, Line};
use rio_vt::crosswords::square::Wide;
use rio_vt::crosswords::style::StyleFlags;
use rio_vt::event::EventListener;

use crate::core::CellData;

/// The palette index Roost's wire uses for "whatever the terminal's default is".
const DEFAULT_COLOR: u16 = 256;

/// The kitty graphics protocol's Unicode placeholder for a virtual placement.
pub(crate) const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

/// A per-core prefix, so a `link_key` names a link run within ONE terminal.
///
/// The same hyperlink always produces the same key, which is what lets a run
/// of cells fold into one span and two adjacent runs of the same link stay two
/// spans, without mutating anything to read a cell.
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

/// One cell, in the shape the emitter reads.
pub(crate) fn cell_data<U: EventListener>(
    term: &Crosswords<U>,
    line: Line,
    column: Column,
    links: &LinkScope,
) -> CellData {
    let grid = &term.grid;
    let square = grid[line][column];
    let style = grid.style_of(&square);
    let (fg, fg_rgb) = palette(term, style.fg);
    let (bg, bg_rgb) = palette(term, style.bg);
    let (link_uri, link_key) = match term.cell_hyperlink(line, column) {
        Some(hyperlink) => (
            Some(hyperlink.uri().to_owned()),
            Some(links.key_for(hyperlink.id())),
        ),
        None => (None, None),
    };
    let (character, width) = match square.wide() {
        Wide::Wide => (square.c(), 2),
        Wide::Spacer | Wide::LeadingSpacer => ('\0', 0),
        Wide::Narrow => match square.c() {
            '\0' | KITTY_PLACEHOLDER => (' ', 1),
            other => (other, 1),
        },
    };
    let combining = if width == 0 || square.c() == KITTY_PLACEHOLDER {
        None
    } else {
        square
            .extras_id_checked()
            .and_then(|id| grid.extras_table.get(id))
            .filter(|extras| !extras.zerowidth.is_empty())
            .map(|extras| extras.zerowidth.iter().map(|c| *c as u32).collect())
    };
    CellData {
        character: character as u32,
        combining,
        width,
        fg,
        bg,
        flags: roost_flags(style.flags),
        fg_rgb,
        bg_rgb,
        link_uri,
        link_key,
    }
}

/// A colour as the wire carries it: a palette index, or a true-colour value.
fn palette<U: EventListener>(term: &Crosswords<U>, color: AnsiColor) -> (u16, Option<u32>) {
    match color {
        AnsiColor::Indexed(index) => (u16::from(index), None),
        AnsiColor::Spec(rgb) => (
            DEFAULT_COLOR,
            Some((u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b)),
        ),
        AnsiColor::Named(named) => match term.colors()[named] {
            Some(rgba) => (DEFAULT_COLOR, Some(packed(rgba))),
            None => match u16::try_from(named as usize) {
                Ok(index) if index < ANSI_NAMED_COLORS => (index, None),
                _ => (DEFAULT_COLOR, None),
            },
        },
    }
}

/// How many `NamedColor`s are plain ANSI palette entries: `Black` (0) through
/// `LightWhite` (15).
const ANSI_NAMED_COLORS: u16 = 16;

/// Rio stores an override as normalized RGBA floats.
fn packed(rgba: ColorArray) -> u32 {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(rgba[0]) << 16) | (channel(rgba[1]) << 8) | channel(rgba[2])
}

/// Rio's style flags as the wire's `CELL_*` bitfield.
///
/// `CELL_BLINK` is never set: blink is a cursor concern for Roost, and the
/// terminal-core conformance family records the difference from v2.
fn roost_flags(flags: StyleFlags) -> u16 {
    use roost_protocol::cell::{
        CELL_BOLD, CELL_DIM, CELL_INVISIBLE, CELL_ITALIC, CELL_REVERSE, CELL_STRIKE, CELL_UNDERLINE,
    };
    let mut wire = 0u16;
    for (style, bit) in [
        (StyleFlags::BOLD, CELL_BOLD),
        (StyleFlags::DIM, CELL_DIM),
        (StyleFlags::ITALIC, CELL_ITALIC),
        (StyleFlags::INVERSE, CELL_REVERSE),
        (StyleFlags::HIDDEN, CELL_INVISIBLE),
        (StyleFlags::STRIKEOUT, CELL_STRIKE),
    ] {
        if flags.contains(style) {
            wire |= bit;
        }
    }
    // `ALL_UNDERLINES` is a composite of five one-hot kinds, so `intersects`.
    if flags.intersects(StyleFlags::ALL_UNDERLINES) {
        wire |= CELL_UNDERLINE;
    }
    wire
}

/// Mouse tracking as the wire's closed set: `1000`, `1002`, or none.
///
/// `MOUSE_MOTION` is the `1003` any-motion mode, which the v2 core folded away
/// and so does this one: a client that received it would have to report motion
/// for a terminal the product does not forward it for.
pub(crate) fn mouse_tracking(mode: Mode) -> roost_protocol::cell::MouseTracking {
    use roost_protocol::cell::MouseTracking;
    if mode.contains(Mode::MOUSE_DRAG) {
        MouseTracking::ButtonMotion
    } else if mode.contains(Mode::MOUSE_REPORT_CLICK) {
        MouseTracking::PressRelease
    } else {
        MouseTracking::None
    }
}
