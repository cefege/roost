//! Where the open select listbox sits: under its trigger, the trigger's width,
//! flipped above when it would run off the bottom and there is more room above.
//! Called by `select_listbox.rs`; depends on nothing.
//!
//! v2's Kobalte select placed its content with a floating positioner
//! (`placement: "bottom-start"`, `gutter: 8`, `sameWidth: true`, flip on
//! overflow). This is the same geometry as a `position: fixed` style computed
//! from the trigger's client rect.

/// The gap between the trigger and the listbox, in CSS pixels.
pub const SELECT_GUTTER_PX: f64 = 8.0;

/// The trigger's client rect, in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnchorRect {
    /// Left edge.
    pub left: f64,
    /// Top edge.
    pub top: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
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

/// Place the listbox. Until its height and the viewport are known it opens
/// below; once known, it flips above only when it overflows below AND above has
/// more room, so a listbox that fits nowhere stays where the eye expects it.
pub fn listbox_placement(
    anchor: AnchorRect,
    content_height: Option<f64>,
    viewport_height: Option<f64>,
) -> ListboxPlacement {
    let below_top = anchor.top + anchor.height + SELECT_GUTTER_PX;
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
    let room_above = anchor.top - SELECT_GUTTER_PX;
    if height > room_below && room_above > room_below {
        ListboxPlacement {
            top: anchor.top - SELECT_GUTTER_PX - height,
            side: PlacementSide::Top,
            ..below
        }
    } else {
        below
    }
}

/// The content element's inline style for a placement. The transform origin is
/// the edge nearest the trigger, which `controls.css` reads for the open
/// animation.
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
