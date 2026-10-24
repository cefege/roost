//! The `/design` route is a live catalogue of Libero controls and Roost's visual roles.

use dioxus::prelude::*;
use libero::{
    components::{Box, Flex, NavLink, Paper, ScrollArea, Sidebar, Text, Title},
    sx::sx,
};

use super::theme_roles::ThemeRoles;
use super::{
    content_primitives::ContentPrimitives, control_states::DesignControlStates,
    overlay_states::DesignOverlayStates,
};
use crate::components::settings::theme::ThemePane;
use crate::components::settings_navigation::SETTINGS_GROUPS;

pub const DESIGN_GALLERY_TEST_ID: &str = "design-gallery";

#[component]
pub fn DesignGallery(on_navigate: EventHandler<String>) -> Element {
    let page_sx = use_hook(|| {
        sx().padding("md")
            .gap("lg")
            .width("100%")
            .height("100%")
            .min_height("0")
            .overflow_y("auto")
            .max_width("88rem")
            .margin_inline("auto")
            .box_sizing("border-box")
    });
    let section_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .padding("md")
            .gap("md")
            .min_width("0")
    });
    let grid_sx = use_hook(|| {
        sx().display("grid")
            .gap("md")
            .align_items("start")
            .grid_template_columns("repeat(auto-fit,minmax(min(100%,24rem),1fr))")
    });
    let navigation_sx = use_hook(|| sx().width("100%").min_height("0"));
    rsx! {
        Flex { direction: "column", sx: page_sx, "data-testid": DESIGN_GALLERY_TEST_ID,
            Flex { direction: "column", gap: "xs",
                Title { component: "h1", size: "xl", "Roost components" }
                Text { size: "sm", color: "muted",
                    "Live controls, shared appearance and compact workbench surfaces."
                }
            }
            Box { sx: grid_sx.clone(),
                Paper { sx: section_sx.clone(), DesignControlStates {} }
                Paper { sx: section_sx.clone(), ContentPrimitives {} }
            }
            Paper { sx: section_sx.clone(), DesignOverlayStates {} }
            Box { sx: grid_sx,
                Paper { sx: section_sx.clone(),
                    Title { component: "h2", size: "lg", "Appearance" }
                    ThemePane {}
                }
                Paper { sx: section_sx.clone(),
                    Title { component: "h2", size: "lg", "Theme roles" }
                    ThemeRoles {}
                }
            }
            Paper { sx: section_sx,
                Title { component: "h2", size: "lg", "Settings navigation" }
                ScrollArea { height: "18rem",
                    Sidebar { sx: navigation_sx,
                        Flex { direction: "column", gap: "xs",
                            for group in SETTINGS_GROUPS {
                                Title { component: "h3", size: "sm", "{group.label}" }
                                for pane in group.panes {
                                    NavLink {
                                        size: "sm",
                                        to: crate::routes::Route::Settings { pane: Some(pane.id.to_owned()) }.to_path(),
                                        onclick: move |event: MouseEvent| {
                                            event.prevent_default();
                                            on_navigate.call(crate::routes::Route::Settings { pane: Some(pane.id.to_owned()) }.to_path());
                                        },
                                        "{pane.label}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
