//! `DesignGallery`: the single visual reference for the Roost design system —
//! the workbench shell specimen, every colour role, the type, spacing, shape and
//! elevation scales, and live examples of every shared `ui` primitive on one
//! `/design` page. `app.rs` routes `/design` here.
//!
//! Colours, radii and type come from the Tailwind theme roles in `tailwind.css`
//! (`bg-card`, `text-muted-foreground`, `rounded-lg`, …), never raw values, so a
//! specimen tracks the theme the product uses.

use dioxus::prelude::*;

use super::content_primitives::ContentPrimitives;
use super::control_states::DesignControlStates;
use super::loader_specimen::LoaderSpecimen;
use super::overlay_states::DesignOverlayStates;
use super::settings_navigation_specimen::SettingsNavigationSpecimen;
use super::token_sections::{ColorRoles, ElevationScale, ShapeScale, SpacingScale, TypeRamp};
use super::workbench_shell_specimen::WorkbenchShellSpecimen;
use crate::components::ui::{Surface, SurfaceRadius};

/// The gallery root's `data-testid`, the one `app::ServedSurface::Design` names.
pub const DESIGN_GALLERY_TEST_ID: &str = "design-gallery";

/// The page's own frame: full height, its own scroll, the base surface.
const GALLERY_CLASS: &str =
    "box-border min-h-screen overflow-y-auto bg-background p-6 text-foreground";

/// A gallery section's title.
const SECTION_HEADER_CLASS: &str = "mt-0 mb-4 text-xl font-semibold text-foreground";

/// A gallery section's lead paragraph.
const LEAD_TEXT_CLASS: &str = "mt-0 mb-4 text-sm text-muted-foreground";

/// A gallery section's own surface, spaced from the next section.
const SECTION_SURFACE_CLASS: &str = "mb-6 block";

/// A gallery section's title, rendered as an `h2`.
#[component]
pub fn GallerySectionHeader(children: Element) -> Element {
    rsx! {
        h2 { class: SECTION_HEADER_CLASS, {children} }
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
            class: SECTION_SURFACE_CLASS,
            GallerySectionHeader { {title} }
            {children}
        }
    }
}

/// The `/design` page. `on_navigate` carries the specimen rail's in-app links.
#[component]
pub fn DesignGallery(on_navigate: EventHandler<String>) -> Element {
    rsx! {
        div { class: GALLERY_CLASS, "data-testid": DESIGN_GALLERY_TEST_ID,
            header { class: "mb-6",
                h1 { class: "m-0 text-3xl font-semibold text-foreground", "Design system" }
                p { class: "mt-2 mb-0 text-base text-muted-foreground",
                    "Every token + primitive on one page. Colors + type are token-only."
                }
            }
            GallerySection { title: "Canonical workbench shell",
                p { class: LEAD_TEXT_CLASS,
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
            GallerySection { title: "Loader", LoaderSpecimen {} }
            GallerySection { title: "Content primitives", ContentPrimitives {} }
            GallerySection { title: "Overlay states", DesignOverlayStates {} }
        }
    }
}
