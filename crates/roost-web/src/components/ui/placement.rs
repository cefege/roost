//! Where a floating panel goes relative to its trigger: the anchored menu panel
//! and the select listbox, in one `ClientRect` space. Pure geometry; called by
//! `anchored_panel` and `select_listbox`, which measure and apply the result.
//! Menu positions come out in the containing block's coordinates, because a
//! transformed deck is the containing block of every `position: fixed` popup.

/// Space kept between a popup and the edge of its bounds.
pub const EDGE_MARGIN_PX: f64 = 8.0;
/// Gap between the trigger and the popup.
pub const TRIGGER_GAP_PX: f64 = 4.0;
/// A side with at least this much room is used without comparing the other.
const COMFORTABLE_HEIGHT_PX: f64 = 240.0;

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
