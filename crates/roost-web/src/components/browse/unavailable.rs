//! The picker's answer to a machine this coordinator does not have: a status
//! region the reader can reach and a way home, painted INSTEAD of the entry
//! grid. It is a focusable region rather than a card in the flow because the
//! route that produced it names a machine no registry row will ever match, and a
//! reader who lands here has to be able to leave with the keyboard.
//!
//! Called by `browse::picker::page`. Ports the `scopeUnavailable` branch of
//! `apps/web/src/components/browse/WorkerBrowsePage.tsx`.

use dioxus::prelude::*;

use libero::components::{Box, Button, Flex, Pictogram, Text};
use libero::sx::sx;
/// The region's element id, so the page can put focus on it when it appears.
pub const REGION_ID: &str = "browse-worker-unavailable";

/// The accessible name the region announces.
pub const LABEL: &str = "Machine unavailable. This machine isn't available on this coordinator.";

/// The machine-unavailable panel.
#[component]
pub fn MachineUnavailable(on_go_home: EventHandler<()>) -> Element {
    let region_sx = use_hook(|| {
        sx().display("flex")
            .min_height("0")
            .flex("1")
            .align_items("center")
            .justify_content("center")
            .padding("1.5rem")
            .outline("none")
    });
    let content_sx = use_hook(|| sx().text_align("center"));
    let icon_sx = use_hook(|| sx().width("2rem").height("2rem").color("muted"));
    rsx! {
        Box {
            sx: region_sx,
            id: REGION_ID,
            "data-testid": REGION_ID,
            role: "status",
            "aria-live": "polite",
            "aria-label": LABEL,
            "aria-atomic": "true",
            tabindex: "-1",
            Flex { direction: "column", align: "center", gap: "sm", sx: content_sx,
                Box { component: "span", sx: icon_sx, Pictogram { icon: pictogram_icons_lucide::folder_x::outlined, width: "2rem", height: "2rem" } }
                Text { size: "lg", "Machine unavailable" }
                Text { size: "sm", color: "muted", "This machine isn't available on this coordinator." }
                Button {
                    variant: "outlined",
                    "data-testid": "browse-worker-unavailable-home",
                    onclick: move |_| on_go_home.call(()),
                    "Go home"
                }
            }
        }
    }
}
