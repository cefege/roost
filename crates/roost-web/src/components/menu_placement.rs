//! Where a popup anchored to a trigger goes so it always fits: above or below,
//! whichever side has room, aligned to the trigger edge that keeps it inside
//! its bounds, with a height cap that makes a long list scroll instead of
//! running off the pane. Pure; `agent_chat::toolbar_menu` measures and calls it.
//! Positions come out in the containing block's coordinates, because a
//! transformed deck is the containing block of every `position: fixed` popup.

use crate::components::deck::deck_dom::ClientBox;

/// Space kept between a popup and the edge of its bounds.
pub const EDGE_MARGIN_PX: f64 = 8.0;
/// Gap between the trigger and the popup.
pub const TRIGGER_GAP_PX: f64 = 4.0;
/// A side with at least this much room is used without comparing the other.
const COMFORTABLE_HEIGHT_PX: f64 = 240.0;

/// The popup's inline geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MenuPlacement {
    /// `top` when it drops, `bottom` when it rises, in containing-block px.
    pub vertical: Vertical,
    pub horizontal: Horizontal,
    /// The room on the chosen side; the list scrolls past it.
    pub max_height: f64,
    /// The room from the anchored edge to the opposite bounds margin.
    pub max_width: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Vertical {
    Top(f64),
    Bottom(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Horizontal {
    Left(f64),
    Right(f64),
}

impl MenuPlacement {
    /// The geometry as inline CSS longhands; every property is named in every
    /// branch so a re-render never keeps a stale edge.
    pub fn style(&self) -> String {
        let (top, bottom) = match self.vertical {
            Vertical::Top(top) => (format!("{top}px"), "auto".to_owned()),
            Vertical::Bottom(bottom) => ("auto".to_owned(), format!("{bottom}px")),
        };
        let (left, right) = match self.horizontal {
            Horizontal::Left(left) => (format!("{left}px"), "auto".to_owned()),
            Horizontal::Right(right) => ("auto".to_owned(), format!("{right}px")),
        };
        format!(
            "top: {top}; bottom: {bottom}; left: {left}; right: {right}; \
             max-height: {}px; max-width: {}px; overflow-y: auto;",
            self.max_height, self.max_width
        )
    }
}

/// Place a popup for `trigger` inside `bounds` (both in viewport px), written
/// relative to `containing` (the transformed ancestor's box in viewport px, or
/// the viewport itself). `prefer_up` wins whenever its side has room.
pub fn place_menu(
    trigger: ClientBox,
    bounds: ClientBox,
    containing: ClientBox,
    prefer_up: bool,
) -> MenuPlacement {
    let above = (trigger.top - bounds.top - EDGE_MARGIN_PX - TRIGGER_GAP_PX).max(0.0);
    let below = (bounds.bottom() - trigger.bottom() - EDGE_MARGIN_PX - TRIGGER_GAP_PX).max(0.0);
    let (preferred, other) = if prefer_up {
        (above, below)
    } else {
        (below, above)
    };
    let keep = preferred >= COMFORTABLE_HEIGHT_PX || preferred >= other;
    let up = prefer_up == keep;
    let vertical = if up {
        Vertical::Bottom(containing.bottom() - trigger.top + TRIGGER_GAP_PX)
    } else {
        Vertical::Top(trigger.bottom() + TRIGGER_GAP_PX - containing.top)
    };
    // A trigger in the left half opens rightward, so a picker at the start of
    // a bar never pushes its menu past the pane's left edge.
    let opens_right = trigger.left + trigger.width / 2.0 < bounds.left + bounds.width / 2.0;
    let (horizontal, max_width) = if opens_right {
        let left = trigger.left.max(bounds.left + EDGE_MARGIN_PX);
        (
            Horizontal::Left(left - containing.left),
            bounds.right() - EDGE_MARGIN_PX - left,
        )
    } else {
        let right = trigger.right().min(bounds.right() - EDGE_MARGIN_PX);
        (
            Horizontal::Right(containing.right() - right),
            right - bounds.left - EDGE_MARGIN_PX,
        )
    };
    MenuPlacement {
        vertical,
        horizontal,
        max_height: if up { above } else { below },
        max_width: max_width.max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(left: f64, top: f64, width: f64, height: f64) -> ClientBox {
        ClientBox {
            left,
            top,
            width,
            height,
        }
    }

    const VIEWPORT: ClientBox = ClientBox {
        left: 0.0,
        top: 0.0,
        width: 1400.0,
        height: 900.0,
    };

    #[test]
    fn a_composer_picker_rises_into_the_room_above_it_and_caps_there() {
        let deck = boxed(350.0, 40.0, 1050.0, 860.0);
        let picker = boxed(460.0, 840.0, 120.0, 28.0);
        let placement = place_menu(picker, deck, deck, true);
        assert_eq!(placement.vertical, Vertical::Bottom(900.0 - 840.0 + 4.0));
        assert_eq!(placement.max_height, 840.0 - 40.0 - 8.0 - 4.0);
        // Left half of the deck: anchored at the picker's left, in deck px.
        assert_eq!(placement.horizontal, Horizontal::Left(110.0));
    }

    #[test]
    fn a_short_space_above_flips_the_menu_below() {
        let picker = boxed(900.0, 60.0, 120.0, 28.0);
        let placement = place_menu(picker, VIEWPORT, VIEWPORT, true);
        assert!(matches!(placement.vertical, Vertical::Top(_)));
        assert_eq!(placement.max_height, 900.0 - 88.0 - 12.0);
    }

    #[test]
    fn the_preferred_side_wins_when_it_is_comfortable() {
        let picker = boxed(900.0, 300.0, 120.0, 28.0);
        let placement = place_menu(picker, VIEWPORT, VIEWPORT, true);
        assert!(matches!(placement.vertical, Vertical::Bottom(_)));
    }

    #[test]
    fn a_trigger_on_the_right_aligns_right_edges_inside_the_margin() {
        let picker = boxed(1350.0, 300.0, 60.0, 28.0);
        let placement = place_menu(picker, VIEWPORT, VIEWPORT, false);
        assert_eq!(placement.horizontal, Horizontal::Right(8.0));
    }

    #[test]
    fn inset_triggers_keep_wide_popups_inside_both_viewport_margins() {
        let viewport = boxed(0.0, 0.0, 320.0, 740.0);
        for trigger_left in [17.0, 271.0] {
            let trigger = boxed(trigger_left, 620.0, 32.0, 32.0);
            let placement = place_menu(trigger, viewport, viewport, true);
            let popup_width = 320.0_f64.min(placement.max_width);
            let popup_left = match placement.horizontal {
                Horizontal::Left(left) => left,
                Horizontal::Right(right) => viewport.right() - right - popup_width,
            };
            assert!(popup_left >= viewport.left + EDGE_MARGIN_PX);
            assert!(popup_left + popup_width <= viewport.right() - EDGE_MARGIN_PX);
        }
    }
}
