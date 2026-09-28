//! `DesignGallery`: the single visual reference for the Roost design system —
//! the workbench shell specimen, every colour token, the type, spacing, shape
//! and elevation scales, and live examples of every shared primitive on one
//! `/design` page. Ported from `apps/web/src/components/design/DesignGallery.tsx`;
//! `app.rs` routes `/design` here.
//!
//! HARD RULE: colours and font sizes come ONLY from theme tokens via `var(--…)`.
//! The raw-value ratchet (`xtask/src/design_raw.rs`) fails a raw hex, `rgb()` or
//! px font size in this tree.

use dioxus::prelude::*;

use super::content_primitives::ContentPrimitives;
use super::control_states::DesignControlStates;
use super::overlay_states::DesignOverlayStates;
use super::settings_navigation_specimen::SettingsNavigationSpecimen;
use super::token_sections::{ColorRoles, ElevationScale, ShapeScale, SpacingScale, TypeRamp};
use super::workbench_shell_specimen::WorkbenchShellSpecimen;
use crate::components::md::{Surface, SurfaceRadius};

/// The gallery root's `data-testid`, the one `app::ServedSurface::Design` names.
pub const DESIGN_GALLERY_TEST_ID: &str = "design-gallery";

/// The page's own frame: full height, its own scroll, the base surface.
const GALLERY_STYLE: &str = "min-height: 100vh; overflow-y: auto; background: var(--bg-base); \
     color: var(--text-hi); padding: var(--md-space-6); box-sizing: border-box;";

/// A gallery section's title, in the title-large ramp.
#[component]
pub fn GallerySectionHeader(children: Element) -> Element {
    rsx! {
        h2 {
            style: "color: var(--text-hi); margin: 0 0 var(--md-space-4); \
                    font-size: var(--md-title-l-size); line-height: var(--md-title-l-line); \
                    font-weight: var(--md-title-l-weight);",
            {children}
        }
    }
}

/// One titled gallery section on its own elevated surface.
#[component]
pub fn GallerySection(title: String, children: Element) -> Element {
    rsx! {
        Surface {
            level: 1,
            elevation: 1,
            radius: SurfaceRadius::Lg,
            pad: 6,
            border: true,
            style: "display: block; margin-bottom: var(--md-space-6);",
            GallerySectionHeader { {title} }
            {children}
        }
    }
}

/// The `/design` page. `on_navigate` carries the specimen rail's in-app links.
#[component]
pub fn DesignGallery(on_navigate: EventHandler<String>) -> Element {
    rsx! {
        div { style: GALLERY_STYLE, "data-testid": DESIGN_GALLERY_TEST_ID,
            header { style: "margin-bottom: var(--md-space-6);",
                h1 {
                    style: "margin: 0; color: var(--text-hi); font-size: var(--md-display-s-size); \
                            line-height: var(--md-display-s-line); font-weight: var(--md-display-s-weight);",
                    "Design system"
                }
                p {
                    style: "margin: var(--md-space-2) 0 0; color: var(--text-mid); \
                            font-size: var(--md-body-m-size); line-height: var(--md-body-m-line);",
                    "Every token + primitive on one page. Colors + type are token-only."
                }
            }
            GallerySection { title: "Canonical workbench shell",
                p {
                    style: "margin: 0 0 var(--md-space-4); color: var(--text-mid); \
                            font-size: var(--md-body-s-size); line-height: var(--md-body-s-line);",
                    "Title bar, activity rail, primary sidebar, terminal editor, and truthful status bar share one contiguous desktop surface."
                }
                WorkbenchShellSpecimen {}
            }
            GallerySection { title: "Settings navigation", SettingsNavigationSpecimen { on_navigate } }
            GallerySection { title: "Color roles", ColorRoles {} }
            GallerySection { title: "Type ramp", TypeRamp {} }
            GallerySection { title: "Spacing scale", SpacingScale {} }
            GallerySection { title: "Shape / radii", ShapeScale {} }
            GallerySection { title: "Elevation", ElevationScale {} }
            GallerySection { title: "Control states", DesignControlStates {} }
            GallerySection { title: "Content primitives", ContentPrimitives {} }
            GallerySection { title: "Overlay states", DesignOverlayStates {} }
        }
    }
}
