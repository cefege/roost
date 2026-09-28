//! The deck's browser adapter: element measurement, the size observers, the
//! one-shot timers, and the window/document listeners a drag, a chord or a
//! swipe needs. Called by `terminal_deck`, `pane_strip` and `pane_divider`.
//! Every rule these feed is in a target-independent sibling; a native build
//! gets inert arms (no element, no listener), because it paints nothing.

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
mod web_listeners;

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
#[cfg(target_arch = "wasm32")]
pub use web::*;
#[cfg(target_arch = "wasm32")]
pub use web_listeners::Listeners;

/// A bounding box in client px.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ClientBox {
    /// Left edge.
    pub left: f64,
    /// Top edge.
    pub top: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl ClientBox {
    /// The bottom edge.
    pub fn bottom(&self) -> f64 {
        self.top + self.height
    }
}

/// What a pointer-down on the deck landed on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeckPointerTarget {
    /// Inside a pane's tab strip, which owns its own pointer handling.
    pub in_strip: bool,
    /// The nearest `[data-pane-id]` ancestor's id.
    pub pane_id: Option<String>,
    /// Inside a link, whose middle click belongs to the browser.
    pub in_link: bool,
}

/// One touch sample the swipe tracker reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeckTouch {
    /// A finger went down; `touches` is how many are down now.
    Start { x: f64, y: f64, touches: u32, at_ms: f64 },
    /// The first finger moved.
    Move { x: f64, y: f64, at_ms: f64 },
    /// The gesture ended or was cancelled.
    End { at_ms: f64 },
}

/// The deck element, in context. Its transform makes it the containing block
/// of every `position: fixed` popup inside it, so a popup placed in viewport
/// coordinates must be shifted into the deck's.
#[derive(Clone, Copy, PartialEq)]
pub struct DeckContainer(pub dioxus::prelude::Signal<Option<std::rc::Rc<dioxus::prelude::MountedData>>>);

impl std::fmt::Debug for DeckContainer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("DeckContainer").finish_non_exhaustive()
    }
}

impl DeckContainer {
    /// The deck's box now; the zero box when no deck is mounted (a popup
    /// outside the deck is already in viewport coordinates).
    pub fn origin(&self) -> ClientBox {
        use dioxus::prelude::*;
        self.0.peek().as_deref().and_then(client_box).unwrap_or_default()
    }
}

/// A right-anchored menu position moved from viewport coordinates into the
/// containing block `container` (whose right edge sits `viewport_width -
/// right` in from the viewport's).
pub fn menu_pos_in(
    pos: crate::components::context_menu::AnchoredMenuPos,
    container: ClientBox,
    viewport_width: f64,
) -> crate::components::context_menu::AnchoredMenuPos {
    if container.width == 0.0 {
        return pos;
    }
    let right_gap = viewport_width - (container.left + container.width);
    crate::components::context_menu::AnchoredMenuPos {
        right: pos.right - right_gap,
        y: pos.y - container.top,
    }
}
