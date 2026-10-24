//! The two panels a route resolves to when there is no surface to draw: one for
//! a path the grammar knows whose surface this build does not carry, and one for
//! a path the grammar does not know at all.
//!
//! They are SEPARATE panels on purpose. "This URL is a surface this build does
//! not carry" and "this URL is not a route" are different facts about the
//! reader's address bar, and collapsing them into one "not found" would make a
//! missing feature read as a typo — or, worse, make a typo read as a missing
//! feature and send someone looking for a release that does not exist.
//!
//! Neither panel navigates anywhere on its own. A panel that offered a guessed
//! destination would be the redirect v2's `*` route performs, which is the
//! behaviour this crate's `Route::Unknown` doc was written to prevent.

use dioxus::prelude::*;
use libero::components::{Anchor, Box, Code, Flex, Paper, Pictogram, Text};
use libero::sx::sx;

use crate::routes::Route;

/// A path the grammar recognises whose surface this build does not carry.
///
/// The path is shown verbatim. A reader who followed a bookmark needs to see
/// what they followed, not a canonicalised version of it.
#[component]
pub fn NotServed(path: String) -> Element {
    let tv = crate::components::layout::window_size::use_tv_layout();
    let base_frame_sx = use_hook(|| {
        sx().display("flex")
            .min_height("100dvh")
            .width("100%")
            .align_items("center")
            .justify_content("center")
            .padding("lg")
    });
    let overscan_sx = use_hook(|| {
        sx().padding_top("var(--tv-overscan-block)")
            .padding_bottom("var(--tv-overscan-block)")
            .padding_left("var(--tv-overscan-inline)")
            .padding_right("var(--tv-overscan-inline)")
    });
    let frame_sx = if tv {
        base_frame_sx.and(overscan_sx)
    } else {
        base_frame_sx
    };
    let card_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("md")
            .padding("md")
    });
    rsx! {
        Box { sx: frame_sx, component: "section", "data-testid": "route-not-served",
            Paper { sx: card_sx, role: "status",
                Pictogram { icon: pictogram_icons_lucide::construction::outlined, width: "1rem", height: "1rem" }
                Flex { direction: "row", wrap: true, gap: "xs",
                    Text { size: "sm", "This build does not carry the surface for" }
                    Code { source: path }
                }
            }
        }
    }
}

/// A path the route grammar does not recognise.
#[component]
pub fn NotFound(path: String, on_navigate: EventHandler<String>) -> Element {
    let home_path = Route::Home.to_path();
    let tv = crate::components::layout::window_size::use_tv_layout();
    let base_frame_sx = use_hook(|| {
        sx().display("flex")
            .min_height("100dvh")
            .width("100%")
            .align_items("center")
            .justify_content("center")
            .padding("lg")
    });
    let overscan_sx = use_hook(|| {
        sx().padding_top("var(--tv-overscan-block)")
            .padding_bottom("var(--tv-overscan-block)")
            .padding_left("var(--tv-overscan-inline)")
            .padding_right("var(--tv-overscan-inline)")
    });
    let frame_sx = if tv {
        base_frame_sx.and(overscan_sx)
    } else {
        base_frame_sx
    };
    let card_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("md")
            .padding("md")
    });
    rsx! {
        Box { sx: frame_sx, component: "section", "data-testid": "route-not-found",
            Paper { sx: card_sx, role: "status",
                Pictogram { icon: pictogram_icons_lucide::link_2_off::outlined, width: "1rem", height: "1rem" }
                Flex { direction: "column", gap: "md",
                    Flex { direction: "row", wrap: true, gap: "xs",
                        Text { size: "sm", "No route matches" }
                        Code { source: path }
                    }
                    Anchor {
                        to: home_path.clone(),
                        onclick: move |_| on_navigate.call(home_path.clone()),
                        "data-testid": "route-not-found-home",
                        "Go to sessions"
                    }
                }
            }
        }
    }
}

/// A path the grammar recognises whose surface this build does not carry.
///
/// The path is shown verbatim. A reader who followed a bookmark needs to see
/// what they followed, not a canonicalised version of it.
#[cfg(test)]
mod tests {
    use crate::app::{ServedSurface, Surface, surface_for};
    use crate::routes::Route;

    #[test]
    fn a_recognised_surface_path_and_a_typo_reach_different_panels() {
        // The two facts a reader has about their address bar, kept apart.
        assert!(matches!(
            surface_for(&Route::parse("/settings/machines")),
            Surface::Served(ServedSurface::Settings)
        ));
        assert!(matches!(
            surface_for(&Route::parse("/setttings")),
            Surface::NotFound { .. }
        ));
    }

    #[test]
    fn a_typo_in_a_known_prefix_is_a_not_found_and_not_a_served_surface() {
        // `/setttings` starts with the right letters. Treating it as a settings
        // surface with a typo would send a reader to a pane they did not open.
        assert!(matches!(
            surface_for(&Route::parse("/setttings/machines")),
            Surface::NotFound { .. }
        ));
    }
}
