//! The Folders panel's contextual empty state: icon, title, body and the call
//! to action that fixes it. Ports
//! `apps/web/src/components/sidebar/SidebarEmptyState.tsx`; `AllView` renders
//! it while no machine is registered.

use dioxus::prelude::*;

use crate::components::ui::empty::EmptyState;
use crate::components::ui::{Button, ButtonVariant, IconType};
use crate::router_state::use_navigate;

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
    rsx! {
        div {
            class: "flex flex-1 items-center justify-center p-4",
            "data-testid": "sidebar-empty-state",
            "data-kind": kind.as_str(),
            EmptyState {
                icon: IconType::MonitorOff,
                title,
                supporting: body,
                action: Some(rsx! {
                    Button {
                        variant: ButtonVariant::Secondary,
                        "data-testid": "sidebar-empty-state-cta",
                        onclick: move |_| navigate.call(cta_href.to_owned()),
                        {cta_label}
                    }
                }),
            }
        }
    }
}
