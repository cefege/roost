//! The shared Roost silhouette used by the activity rail and home heading.
//! The mark inherits currentColor so it follows the surrounding Rust/UI
//! foreground role; its geometry is independent of the palette.

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
            view_box: "0 0 100 100",
            width: size,
            height: size,
            fill: "currentColor",
            "aria-hidden": "true",
            path { d: "M19 53 8 45l4 27 17-3Z" }
            path { d: "m84 32 12 9-12 6Z" }
            path {
                fill_rule: "evenodd",
                d: "M21 55c0-15 9-22 19-24 0-14 9-22 21-22 15 0 24 10 24 25v23c0 20-14 31-34 31-19 0-30-12-30-33Z M40 40c-5 5-9 13-9 23 0 11 8 17 19 17 10 0 18-7 18-17 0-13-9-20-28-23Z M69 24.5a3.5 4.5 0 1 0 0 9 3.5 4.5 0 1 0 0-9Z",
            }
            path { d: "m43 54 8 7-8 7m14 0h8", fill: "none", stroke: "currentColor", stroke_width: "4", stroke_linecap: "round", stroke_linejoin: "round" }
            path { d: "M39 87v7m18-7v7M29 95h13m10 0h13", fill: "none", stroke: "currentColor", stroke_width: "4", stroke_linecap: "round" }
        }
    }
}

/// The size the activity rail brand mark draws at.
pub const ACTIVITY_MARK_SIZE: &str = "1.125rem";

/// The size the home landing's brand mark draws at, and the one its hero uses.
pub const HOME_MARK_SIZE: &str = "1.75rem";

/// The product name displayed on the home landing.
pub const PRODUCT: &str = "Roost";
