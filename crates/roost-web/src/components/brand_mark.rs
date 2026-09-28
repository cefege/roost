//! The Roost brand mark: one SVG, one fill, used wherever the product names
//! itself. Ported from `apps/web/src/components/machines/BrandMark.tsx`, whose
//! geometry is identical to `apps/web/public/icon.svg` so the tab icon and the
//! in-app wordmark stay one mark.
//!
//! The fill is the FIXED brand token, not `--accent`: a brand mark is stable
//! across themes, and retinting it is how a wordmark stops matching its favicon.
//! The colour therefore comes from the stylesheet rule below rather than from a
//! `color:` in the markup, so a caller cannot accidentally restyle it.
//!
//! The primitives are hand-authored overlapping shapes sharing a single fill, so
//! the silhouette is a clean union. A second fill or a stroke would put a
//! winding-rule hole through the body.

use dioxus::prelude::*;

/// The brand's one colour, as a token the stylesheet resolves.
pub const BRAND_COLOR_VAR: &str = "--brand-coral";

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
            class: "brand-mark",
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
