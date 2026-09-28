//! Where a terminal slot paints: its pane's terminal area below the strip, the
//! floated spotlight card, or parked off-screen at the size it will be
//! revealed at. Read by `terminal_deck` for every mounted session. Pure; ports
//! `apps/web/src/components/deck/terminal-deck-geometry.ts` (its slot-equality
//! helpers are Solid memo comparators; a Dioxus prop's `PartialEq` replaces them).

use roost_client_core::deck::{DeckSize, TerminalSessionSlot};

use super::inline_style::{InlineStyle, px};

/// The compact deck bar's height, px.
pub const MOBILE_TERMINAL_STRIP_HEIGHT: f64 = 48.0;

/// The slot's inline style.
///
/// A parked renderer stays LAID OUT at its future viewport size, off-screen and
/// hidden, so its scroll maximum cannot move while frames keep arriving; a
/// `display: none` park would measure zero and repaint at a lying size.
pub fn terminal_session_style(
    slot: Option<&TerminalSessionSlot>,
    park: Option<DeckSize>,
    deck: DeckSize,
    strip_height: f64,
) -> InlineStyle {
    let Some(slot) = slot else {
        let width = park.map_or(if deck.w > 0.0 { deck.w } else { 800.0 }, |park| park.w);
        let height = park.map_or(
            if deck.h > 0.0 { (deck.h - strip_height).max(0.0) } else { 600.0 },
            |park| park.h,
        );
        return InlineStyle::new()
            .with("position", "absolute")
            .with("left", "-99999px")
            .with("top", "0")
            .with("width", px(width))
            .with("height", px(height))
            .with("visibility", "hidden")
            .with("pointer-events", "none");
    };
    let rect = slot.rect;
    if slot.spotlit {
        return InlineStyle::new()
            .with("position", "absolute")
            .with("left", px(rect.x))
            .with("top", px(rect.y))
            .with("width", px(rect.w))
            .with("height", px(rect.h))
            .with("visibility", "inherit")
            .with("z-index", "9")
            .with("overflow", "hidden")
            .with("border-radius", "var(--md-shape-md)");
    }
    InlineStyle::new()
        .with("position", "absolute")
        .with("left", px(rect.x))
        .with("top", px(rect.y + strip_height))
        .with("width", px(rect.w))
        .with("height", px((rect.h - strip_height).max(0.0)))
        .with("visibility", "inherit")
        .with("z-index", if slot.focused { "2" } else { "1" })
}
