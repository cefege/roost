//! Where a floating panel goes relative to its trigger: the anchored menu panel,
//! the select listbox and the dropdown submenu, all in one `ClientRect` space.
//! Pure geometry; called by `anchored_panel`, `select_listbox` and
//! `dropdown_menu_sub::position`, which measure and apply the result.
//! Menu positions come out in the containing block's coordinates, because a
//! transformed deck is the containing block of every `position: fixed` popup.

use super::dropdown_menu_sub::DropdownMenuSubSide;

/// Space kept between a popup and the edge of its bounds.
pub const EDGE_MARGIN_PX: f64 = 8.0;
/// Gap between the trigger and the popup.
pub const TRIGGER_GAP_PX: f64 = 4.0;
/// A side with at least this much room is used without comparing the other.
const COMFORTABLE_HEIGHT_PX: f64 = 240.0;
/// The gap between a submenu row and its panel when it opens sideways.
const SUBMENU_GAP_PX: f64 = 8.0;

/// A bounding box in client px.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ClientRect {
    /// Left edge.
    pub left: f64,
    /// Top edge.
    pub top: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl ClientRect {
    /// The bottom edge.
    pub fn bottom(&self) -> f64 {
        self.top + self.height
    }

    /// The right edge.
    pub fn right(&self) -> f64 {
        self.left + self.width
    }
}

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
    trigger: ClientRect,
    bounds: ClientRect,
    containing: ClientRect,
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

/// Which side of the trigger the listbox opens on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementSide {
    /// Below the trigger.
    Bottom,
    /// Above the trigger.
    Top,
}

/// The listbox's resolved position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ListboxPlacement {
    /// Left edge.
    pub left: f64,
    /// Top edge.
    pub top: f64,
    /// Width: the trigger's.
    pub width: f64,
    /// The side it opened on.
    pub side: PlacementSide,
}

/// Place the select listbox under its trigger at the trigger's width. Until its
/// height and the viewport are known it opens below; once known, it flips above
/// only when it overflows below AND above has more room, so a listbox that fits
/// nowhere stays where the eye expects it.
pub fn listbox_placement(
    anchor: ClientRect,
    content_height: Option<f64>,
    viewport_height: Option<f64>,
) -> ListboxPlacement {
    let below_top = anchor.top + anchor.height + TRIGGER_GAP_PX;
    let below = ListboxPlacement {
        left: anchor.left,
        top: below_top,
        width: anchor.width,
        side: PlacementSide::Bottom,
    };
    let (Some(height), Some(viewport)) = (content_height, viewport_height) else {
        return below;
    };
    let room_below = viewport - below_top;
    let room_above = anchor.top - TRIGGER_GAP_PX;
    if height > room_below && room_above > room_below {
        ListboxPlacement {
            top: anchor.top - TRIGGER_GAP_PX - height,
            side: PlacementSide::Top,
            ..below
        }
    } else {
        below
    }
}

/// The content element's inline style for a placement. The transform origin is
/// the edge nearest the trigger, which the open transition reads.
pub fn listbox_style(placement: &ListboxPlacement) -> String {
    let origin = match placement.side {
        PlacementSide::Bottom => "top",
        PlacementSide::Top => "bottom",
    };
    format!(
        "position: fixed; left: {}px; top: {}px; width: {}px; \
         --kb-select-content-transform-origin: {origin};",
        placement.left, placement.top, placement.width,
    )
}

/// A dropdown submenu's resolved side and viewport-space position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubmenuPlacement {
    pub side: DropdownMenuSubSide,
    pub left: f64,
    pub top: f64,
    pub max_width: f64,
    pub max_height: f64,
}

#[cfg(target_arch = "wasm32")]
impl SubmenuPlacement {
    pub fn style(self) -> String {
        let max_width = (self.max_width - EDGE_MARGIN_PX * 2.0).max(0.0);
        let max_height = (self.max_height - EDGE_MARGIN_PX * 2.0).max(0.0);
        let min_width = 160.0_f64.min(max_width);
        format!(
            "position: fixed; left: {}px; top: {}px; right: auto; bottom: auto; \
             min-width: {min_width}px; max-width: {max_width}px; max-height: {max_height}px; \
             overflow-y: auto;",
            self.left, self.top,
        )
    }
}

/// Resolve a preferred side against the rendered panel's dimensions and
/// viewport. Right/left are preferred; when neither side fits, the menu stacks
/// above or below its row and the panel is clamped inside the viewport.
pub fn place_submenu(
    trigger: ClientRect,
    panel_width: f64,
    panel_height: f64,
    viewport_width: f64,
    viewport_height: f64,
    preferred: DropdownMenuSubSide,
) -> SubmenuPlacement {
    let width = panel_width.max(0.0);
    let height = panel_height.max(0.0);
    let right_room =
        (viewport_width - trigger.left - trigger.width - SUBMENU_GAP_PX - EDGE_MARGIN_PX).max(0.0);
    let left_room = (trigger.left - SUBMENU_GAP_PX - EDGE_MARGIN_PX).max(0.0);
    let below_room =
        (viewport_height - trigger.top - trigger.height - TRIGGER_GAP_PX - EDGE_MARGIN_PX).max(0.0);
    let above_room = (trigger.top - TRIGGER_GAP_PX - EDGE_MARGIN_PX).max(0.0);

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
    let panel_width = width.min((max_width - EDGE_MARGIN_PX * 2.0).max(0.0));
    let panel_height = height.min((max_height - EDGE_MARGIN_PX * 2.0).max(0.0));
    let max_left = (max_width - panel_width - EDGE_MARGIN_PX).max(EDGE_MARGIN_PX);
    let max_top = (max_height - panel_height - EDGE_MARGIN_PX).max(EDGE_MARGIN_PX);

    let (left, top) = match side {
        DropdownMenuSubSide::Right => (
            (trigger.left + trigger.width + SUBMENU_GAP_PX).clamp(EDGE_MARGIN_PX, max_left),
            trigger.top.clamp(EDGE_MARGIN_PX, max_top),
        ),
        DropdownMenuSubSide::Left => (
            (trigger.left - SUBMENU_GAP_PX - panel_width).clamp(EDGE_MARGIN_PX, max_left),
            trigger.top.clamp(EDGE_MARGIN_PX, max_top),
        ),
        DropdownMenuSubSide::Below => (
            trigger.left.clamp(EDGE_MARGIN_PX, max_left),
            (trigger.top + trigger.height + TRIGGER_GAP_PX).clamp(EDGE_MARGIN_PX, max_top),
        ),
        DropdownMenuSubSide::Above => (
            trigger.left.clamp(EDGE_MARGIN_PX, max_left),
            (trigger.top - TRIGGER_GAP_PX - panel_height).clamp(EDGE_MARGIN_PX, max_top),
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
