//! Viewport-space placement for a portaled dropdown submenu. Right/left are
//! preferred; when neither side fits, the menu stacks above or below its row.
//! The panel consumes the remaining viewport instead of escaping its bounds.

use super::dropdown_menu_sub::DropdownMenuSubSide;

const EDGE_INSET: f64 = 8.0;
const HORIZONTAL_GAP: f64 = 8.0;
const VERTICAL_GAP: f64 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SubmenuRect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SubmenuPlacement {
    pub side: DropdownMenuSubSide,
    pub left: f64,
    pub top: f64,
    pub max_width: f64,
    pub max_height: f64,
}

#[cfg(target_arch = "wasm32")]
impl SubmenuPlacement {
    pub fn style(self) -> String {
        let max_width = (self.max_width - EDGE_INSET * 2.0).max(0.0);
        let max_height = (self.max_height - EDGE_INSET * 2.0).max(0.0);
        let min_width = 160.0_f64.min(max_width);
        format!(
            "position: fixed; left: {}px; top: {}px; right: auto; bottom: auto; \
             min-width: {min_width}px; max-width: {max_width}px; max-height: {max_height}px; \
             overflow-y: auto;",
            self.left, self.top,
        )
    }
}

/// Resolve a preferred side against the rendered panel's dimensions and viewport.
pub(super) fn place_submenu(
    trigger: SubmenuRect,
    panel_width: f64,
    panel_height: f64,
    viewport_width: f64,
    viewport_height: f64,
    preferred: DropdownMenuSubSide,
) -> SubmenuPlacement {
    let width = panel_width.max(0.0);
    let height = panel_height.max(0.0);
    let right_room =
        (viewport_width - trigger.left - trigger.width - HORIZONTAL_GAP - EDGE_INSET).max(0.0);
    let left_room = (trigger.left - HORIZONTAL_GAP - EDGE_INSET).max(0.0);
    let below_room =
        (viewport_height - trigger.top - trigger.height - VERTICAL_GAP - EDGE_INSET).max(0.0);
    let above_room = (trigger.top - VERTICAL_GAP - EDGE_INSET).max(0.0);

    let side = match preferred {
        DropdownMenuSubSide::Right => horizontal_side(true, width, right_room, left_room)
            .unwrap_or_else(|| vertical_side(height, below_room, above_room)),
        DropdownMenuSubSide::Left => horizontal_side(false, width, right_room, left_room)
            .unwrap_or_else(|| vertical_side(height, below_room, above_room)),
        DropdownMenuSubSide::Below => vertical_fit_side(true, height, below_room, above_room)
            .unwrap_or_else(|| {
                horizontal_side(true, width, right_room, left_room)
                    .unwrap_or_else(|| wider_side(right_room, left_room))
            }),
        DropdownMenuSubSide::Above => vertical_fit_side(false, height, below_room, above_room)
            .unwrap_or_else(|| {
                horizontal_side(false, width, right_room, left_room)
                    .unwrap_or_else(|| wider_side(right_room, left_room))
            }),
    };

    let max_width = viewport_width.max(0.0);
    let max_height = viewport_height.max(0.0);
    let panel_width = width.min((max_width - EDGE_INSET * 2.0).max(0.0));
    let panel_height = height.min((max_height - EDGE_INSET * 2.0).max(0.0));
    let max_left = (max_width - panel_width - EDGE_INSET).max(EDGE_INSET);
    let max_top = (max_height - panel_height - EDGE_INSET).max(EDGE_INSET);

    let (left, top) = match side {
        DropdownMenuSubSide::Right => (
            (trigger.left + trigger.width + HORIZONTAL_GAP).clamp(EDGE_INSET, max_left),
            trigger.top.clamp(EDGE_INSET, max_top),
        ),
        DropdownMenuSubSide::Left => (
            (trigger.left - HORIZONTAL_GAP - panel_width).clamp(EDGE_INSET, max_left),
            trigger.top.clamp(EDGE_INSET, max_top),
        ),
        DropdownMenuSubSide::Below => (
            trigger.left.clamp(EDGE_INSET, max_left),
            (trigger.top + trigger.height + VERTICAL_GAP).clamp(EDGE_INSET, max_top),
        ),
        DropdownMenuSubSide::Above => (
            trigger.left.clamp(EDGE_INSET, max_left),
            (trigger.top - VERTICAL_GAP - panel_height).clamp(EDGE_INSET, max_top),
        ),
    };

    SubmenuPlacement {
        side,
        left,
        top,
        max_width,
        max_height,
    }
}

fn horizontal_side(
    prefer_right: bool,
    panel_width: f64,
    right_room: f64,
    left_room: f64,
) -> Option<DropdownMenuSubSide> {
    let (preferred_room, other_room) = if prefer_right {
        (right_room, left_room)
    } else {
        (left_room, right_room)
    };
    if preferred_room >= panel_width {
        Some(if prefer_right {
            DropdownMenuSubSide::Right
        } else {
            DropdownMenuSubSide::Left
        })
    } else if other_room >= panel_width {
        Some(if prefer_right {
            DropdownMenuSubSide::Left
        } else {
            DropdownMenuSubSide::Right
        })
    } else {
        None
    }
}

fn vertical_fit_side(
    prefer_below: bool,
    panel_height: f64,
    below_room: f64,
    above_room: f64,
) -> Option<DropdownMenuSubSide> {
    let (preferred_room, other_room) = if prefer_below {
        (below_room, above_room)
    } else {
        (above_room, below_room)
    };
    if preferred_room >= panel_height {
        Some(if prefer_below {
            DropdownMenuSubSide::Below
        } else {
            DropdownMenuSubSide::Above
        })
    } else if other_room >= panel_height {
        Some(if prefer_below {
            DropdownMenuSubSide::Above
        } else {
            DropdownMenuSubSide::Below
        })
    } else {
        None
    }
}

fn vertical_side(panel_height: f64, below_room: f64, above_room: f64) -> DropdownMenuSubSide {
    match vertical_fit_side(true, panel_height, below_room, above_room) {
        Some(side) => side,
        None if below_room >= above_room => DropdownMenuSubSide::Below,
        None => DropdownMenuSubSide::Above,
    }
}

fn wider_side(right_room: f64, left_room: f64) -> DropdownMenuSubSide {
    if right_room >= left_room {
        DropdownMenuSubSide::Right
    } else {
        DropdownMenuSubSide::Left
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: f64, top: f64, width: f64, height: f64) -> SubmenuRect {
        SubmenuRect {
            left,
            top,
            width,
            height,
        }
    }

    #[test]
    fn submenu_prefers_right_and_flips_left_when_right_does_not_fit() {
        let trigger = rect(100.0, 100.0, 120.0, 32.0);
        let right = place_submenu(
            trigger,
            160.0,
            120.0,
            800.0,
            600.0,
            DropdownMenuSubSide::Right,
        );
        assert_eq!(right.side, DropdownMenuSubSide::Right);
        let left = place_submenu(
            rect(650.0, 100.0, 120.0, 32.0),
            160.0,
            120.0,
            800.0,
            600.0,
            DropdownMenuSubSide::Right,
        );
        assert_eq!(left.side, DropdownMenuSubSide::Left);
    }

    #[test]
    fn submenu_stacks_and_caps_inside_a_narrow_viewport() {
        let placement = place_submenu(
            rect(160.0, 30.0, 24.0, 24.0),
            200.0,
            300.0,
            320.0,
            240.0,
            DropdownMenuSubSide::Right,
        );
        assert_eq!(placement.side, DropdownMenuSubSide::Below);
        assert!(placement.left >= EDGE_INSET);
        assert!(placement.top >= EDGE_INSET);
        assert!(placement.max_width <= 320.0);
        assert!(placement.max_height <= 240.0);
    }
}
