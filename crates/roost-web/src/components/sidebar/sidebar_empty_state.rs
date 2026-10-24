//! The Folders panel's contextual empty state: icon, title, body and the call
//! to action that fixes it. Ports
//! `apps/web/src/components/sidebar/SidebarEmptyState.tsx`; `AllView` renders
//! it while no machine is registered.

use crate::router_state::use_navigate;
use dioxus::prelude::*;
use libero::{
    components::{Button, Flex, Paper, Pictogram, Text, Title},
    sx::sx,
};

/// Which empty state. v2's other three kinds (`coord-error`, `search-empty`,
/// `view-empty`) had no caller left in v2 and are not carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyStateKind {
    /// No worker has registered with the coordinator.
    NoMachines,
}

impl EmptyStateKind {
    /// The `data-kind` spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoMachines => "no-machines",
        }
    }

    /// `(title, body)`.
    pub const fn copy(self) -> (&'static str, &'static str) {
        match self {
            Self::NoMachines => (
                "No machines registered",
                "Connect a macOS, Linux, or Windows machine as a worker to start a terminal here.",
            ),
        }
    }

    /// The call to action and where it goes.
    pub const fn call_to_action(self) -> (&'static str, &'static str) {
        match self {
            Self::NoMachines => ("Add a Machine", "/settings/machines"),
        }
    }
}

/// The empty state.
#[component]
pub fn SidebarEmptyState(kind: EmptyStateKind) -> Element {
    let navigate = use_navigate();
    let (title, body) = kind.copy();
    let (cta_label, cta_href) = kind.call_to_action();
    let surface_sx = use_hook(|| {
        sx().display("flex")
            .flex_grow(1)
            .align_items("center")
            .justify_content("center")
            .padding("lg")
    });
    let content_sx = use_hook(|| sx().align_items("center").text_align("center").gap("md"));
    rsx! {
        Paper {
            sx: surface_sx,
            "data-testid": "sidebar-empty-state",
            "data-kind": kind.as_str(),
            Flex { sx: content_sx, direction: "column", gap: "md",
                Pictogram { icon: pictogram_icons_lucide::monitor_off::outlined, width: "1.5rem", height: "1.5rem" }
                Title { size: "md", "{title}" }
                Text { size: "sm", color: "muted", "{body}" }
                Button {
                    variant: "outlined",
                    "data-testid": "sidebar-empty-state-cta",
                    onclick: move |_| navigate.call(cta_href.to_owned()),
                    "{cta_label}"
                }
            }
        }
    }
}
