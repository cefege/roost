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

    /// The right edge.
    pub fn right(&self) -> f64 {
        self.left + self.width
    }
}

/// How far a tab may leave the rail's box before it counts as clipped, px.
///
/// A rail whose width lands on a fraction leaves its tabs' edges inside by less
/// than a pixel; without the slack every rail would report clipped.
pub const CLIP_SLOP_PX: f64 = 1.0;

/// Whether the tab in `tab` is clipped by the rail in `rail`.
///
/// This, not the rail's scroll extent, is the signal the overflow chevron reads.
/// A lifted drag and the reorder spring translate a tab without carrying it out
/// of the rail, and a rail wide enough for every tab still reports a scroll
/// extent past its own client width; either parks the chevron on a rail where
/// nothing is out of reach. Only a tab's own box against the rail's answers the
/// question the chevron exists for.
pub fn clips_rail(rail: ClientBox, tab: ClientBox) -> bool {
    tab.right() > rail.right() + CLIP_SLOP_PX || tab.left < rail.left - CLIP_SLOP_PX
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
    Start {
        x: f64,
        y: f64,
        touches: u32,
        at_ms: f64,
    },
    /// The first finger moved.
    Move { x: f64, y: f64, at_ms: f64 },
    /// The finger lifted.
    End { at_ms: f64 },
    /// The browser took the touch (a system gesture, a scroll it claimed).
    Cancel,
}

/// The deck element, in context. Its transform makes it the containing block
/// of every `position: fixed` popup inside it, so a popup placed in viewport
/// coordinates must be shifted into the deck's.
#[derive(Clone, Copy, PartialEq)]
pub struct DeckContainer(
    pub dioxus::prelude::Signal<Option<std::rc::Rc<dioxus::prelude::MountedData>>>,
);

impl std::fmt::Debug for DeckContainer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeckContainer")
            .finish_non_exhaustive()
    }
}

impl DeckContainer {
    /// The deck's box now; the zero box when no deck is mounted (a popup
    /// outside the deck is already in viewport coordinates).
    pub fn origin(&self) -> ClientBox {
        use dioxus::prelude::*;
        self.0
            .peek()
            .as_deref()
            .and_then(client_box)
            .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the desktop editor region starts in the measured shell.
    const RAIL_LEFT: f64 = 352.0;
    /// `--workbench-tab-width-min`: the width a tab rests at in a packed rail.
    const FLOOR_PX: f64 = 68.0;

    fn rail_box(width: f64) -> ClientBox {
        ClientBox {
            left: RAIL_LEFT,
            top: 35.0,
            width,
            height: 35.0,
        }
    }

    fn tab_box(index: f64, width: f64) -> ClientBox {
        ClientBox {
            left: RAIL_LEFT + index * width,
            top: 35.0,
            width,
            height: 35.0,
        }
    }

    #[test]
    fn six_tabs_at_the_floor_are_clipped_exactly_where_the_row_leaves_the_rail() {
        // 800px desktop: six tabs on their 68px floor are 408px in a 336px rail.
        // That 72px of overflow spans more than one tab, so the last two leave the
        // rail and the chevron is the only way to reach them.
        let rail = rail_box(336.0);
        let clipped = [false, false, false, false, true, true];
        for (index, expected) in clipped.iter().enumerate() {
            let tab = tab_box(index as f64, FLOOR_PX);
            assert_eq!(clips_rail(rail, tab), *expected, "tab {index}");
        }
    }

    #[test]
    fn six_tabs_that_fit_are_not_clipped_when_the_scroll_extent_overruns() {
        // 1024px desktop: the tabs fill the rail and its scroll extent rounds a
        // fraction past its own client width. Nothing is out of reach, so the
        // chevron must stay unmounted.
        let rail = rail_box(473.6);
        for index in 0..6 {
            let tab = tab_box(index as f64, 78.9);
            assert!(!clips_rail(rail, tab), "tab {index} is inside the rail");
        }
    }

    #[test]
    fn a_tab_scrolled_out_to_the_left_is_clipped() {
        // The rail is scrolled to its end, so the first tab's box begins to the
        // left of the rail's own left edge.
        let scrolled_out = ClientBox {
            left: RAIL_LEFT - 52.0,
            top: 35.0,
            width: 200.0,
            height: 35.0,
        };
        assert!(clips_rail(rail_box(400.0), scrolled_out));
    }

    #[test]
    fn a_tab_flush_with_the_rail_edge_is_not_clipped() {
        let rail = rail_box(400.0);
        assert!(!clips_rail(rail, tab_box(0.0, 400.0)));
        assert!(!clips_rail(rail, tab_box(0.0, 399.2)));
    }
}
