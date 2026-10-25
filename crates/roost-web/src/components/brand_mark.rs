//! The shared Roost silhouette used by the title bar and home heading.
//! The mark inherits currentColor so it follows the surrounding Rust/UI
//! foreground role in both themes; its geometry is independent of the palette.

use dioxus::prelude::*;

/// The mark, sized by the surrounding type ramp rather than by a pixel count.
///
/// `size` is a CSS length, and the two callers pass a token-backed one, so the
/// mark scales with the surface instead of being pinned at two hand-picked sizes
/// that drift apart.
#[component]
pub fn BrandMark(size: &'static str) -> Element {
    rsx! {
        svg {
            view_box: "0 0 24 24",
            width: size,
            height: size,
            fill: "currentColor",
            "aria-hidden": "true",
            ellipse { cx: "11.5", cy: "12.5", rx: "6.2", ry: "6.4" }
            circle { cx: "14.5", cy: "8", r: "3.6" }
            polygon { points: "17.5,6.4 19.9,8.8 17.5,11.2" }
            polygon { points: "8.5,10.5 4.0,18.6 11.0,19.0" }
            rect { x: "4.8", y: "19", width: "14.4", height: "1.5", rx: "0.75" }
        }
    }
}

/// The size the title bar's brand mark draws at.
///
/// A token, because the rule is "the mark matches the word beside it". v2 passed
/// `18` here and `28` on the home landing; expressing it as a token means the
/// wordmark and the word it sits beside cannot disagree after a type-ramp change.
pub const TITLE_BAR_MARK_SIZE: &str = "1.125rem";

/// The size the home landing's brand mark draws at, and the one its hero uses.
pub const HOME_MARK_SIZE: &str = "1.75rem";
