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
///
/// A `compact` slot spans the deck below its bar, so its bottom edge is the
/// deck's and CSS resolves it from the deck's CURRENT box. A px height there
/// is the deck's last measured height, which trails a route change that
/// resizes the deck by one observer delivery: a pane revealed in that gap
/// published the stale size, and the next delivery revised it at once.
///
/// Every branch states every property any branch sets. Dioxus 0.7 MERGES a new
/// `style` string into the element's old inline style, keeping each property
/// the new string omits, so a property only one branch states outlives the
/// state that set it: a revealed slot kept the park's `pointer-events: none`.
pub fn terminal_session_style(
    slot: Option<&TerminalSessionSlot>,
    park: Option<DeckSize>,
    deck: DeckSize,
    strip_height: f64,
    compact: bool,
) -> InlineStyle {
    let Some(slot) = slot else {
        let width = park.map_or(if deck.w > 0.0 { deck.w } else { 800.0 }, |park| park.w);
        let height = park.map_or(
            if deck.h > 0.0 {
                (deck.h - strip_height).max(0.0)
            } else {
                600.0
            },
            |park| park.h,
        );
        return SlotStyle {
            left: "-99999px".to_owned(),
            top: "0".to_owned(),
            width: px(width),
            height: px(height),
            bottom: "auto",
            visibility: "hidden",
            pointer_events: "none",
            z_index: "auto",
            overflow: "visible",
            border_radius: "0",
        }
        .into_inline();
    };
    let rect = slot.rect;
    if slot.spotlit {
        return SlotStyle {
            left: px(rect.x),
            top: px(rect.y),
            width: px(rect.w),
            height: px(rect.h),
            bottom: "auto",
            visibility: "inherit",
            pointer_events: "auto",
            z_index: "9",
            overflow: "hidden",
            border_radius: "var(--md-shape-md)",
        }
        .into_inline();
    }
    let (height, bottom) = if compact {
        ("auto".to_owned(), "0px")
    } else {
        (px((rect.h - strip_height).max(0.0)), "auto")
    };
    SlotStyle {
        left: px(rect.x),
        top: px(rect.y + strip_height),
        width: px(rect.w),
        height,
        bottom,
        visibility: "inherit",
        pointer_events: "auto",
        z_index: if slot.focused { "2" } else { "1" },
        overflow: "visible",
        border_radius: "0",
    }
    .into_inline()
}

/// One slot placement with every property a placement may set, so no branch
/// can leave one out.
struct SlotStyle {
    left: String,
    top: String,
    width: String,
    height: String,
    bottom: &'static str,
    visibility: &'static str,
    pointer_events: &'static str,
    z_index: &'static str,
    overflow: &'static str,
    border_radius: &'static str,
}

impl SlotStyle {
    fn into_inline(self) -> InlineStyle {
        InlineStyle::new()
            .with("position", "absolute")
            .with("left", self.left)
            .with("top", self.top)
            .with("width", self.width)
            .with("height", self.height)
            .with("bottom", self.bottom)
            .with("visibility", self.visibility)
            .with("pointer-events", self.pointer_events)
            .with("z-index", self.z_index)
            .with("overflow", self.overflow)
            .with("border-radius", self.border_radius)
    }
}
