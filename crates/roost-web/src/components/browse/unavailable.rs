//! The picker's answer to a machine this coordinator does not have: a status
//! region the reader can reach and a way home, painted INSTEAD of the entry
//! grid. It is a focusable region rather than a card in the flow because the
//! route that produced it names a machine no registry row will ever match, and a
//! reader who lands here has to be able to leave with the keyboard.
//!
//! Called by `browse::picker::page`. Ports the `scopeUnavailable` branch of
//! `apps/web/src/components/browse/WorkerBrowsePage.tsx`.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonVariant, EmptyState};

/// The region's element id, so the page can put focus on it when it appears.
pub const REGION_ID: &str = "browse-worker-unavailable";

/// The accessible name the region announces.
pub const LABEL: &str = "Machine unavailable. This machine isn't available on this coordinator.";

/// The machine-unavailable panel.
#[component]
pub fn MachineUnavailable(on_go_home: EventHandler<()>) -> Element {
    rsx! {
        div {
            class: "df-browse-area",
            id: REGION_ID,
            "data-testid": REGION_ID,
            role: "status",
            "aria-live": "polite",
            "aria-label": LABEL,
            "aria-atomic": "true",
            tabindex: "-1",
            EmptyState {
                icon: "folder_off".to_owned(),
                title: "Machine unavailable".to_owned(),
                supporting: Some("This machine isn't available on this coordinator.".to_owned()),
                action: rsx! {
                    Button {
                        variant: ButtonVariant::Secondary,
                        "data-testid": "browse-worker-unavailable-home",
                        onclick: move |_| on_go_home.call(()),
                        "Go home"
                    }
                },
            }
        }
    }
}
