//! The Folders panel's contextual empty state: icon, title, body and the call
//! to action that fixes it. Ports
//! `apps/web/src/components/sidebar/SidebarEmptyState.tsx`; `AllView` renders
//! it while no machine is registered.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonVariant};
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
            "data-testid": "sidebar-empty-state",
            "data-kind": kind.as_str(),
            style: "display: flex; flex-direction: column; align-items: center; gap: var(--md-space-3); \
                    padding: var(--md-space-7) var(--md-space-4); text-align: center; color: var(--text-lo);",
            div {
                "aria-hidden": "true",
                style: "width: var(--md-space-8); height: var(--md-space-8); color: var(--text-lo); \
                        display: flex; align-items: center; justify-content: center;",
                svg {
                    width: "36",
                    height: "36",
                    view_box: "0 0 24 24",
                    fill: "none",
                    stroke: "currentColor",
                    stroke_width: "1.5",
                    stroke_linecap: "round",
                    stroke_linejoin: "round",
                    rect { x: "2", y: "3", width: "20", height: "14", rx: "2", ry: "2" }
                    line { x1: "8", y1: "21", x2: "16", y2: "21" }
                    line { x1: "12", y1: "17", x2: "12", y2: "21" }
                    line { x1: "2", y1: "2", x2: "22", y2: "22" }
                }
            }
            div {
                style: "font-size: var(--md-label-l-size); color: var(--text-hi); font-weight: var(--md-label-l-weight);",
                {title}
            }
            div {
                style: "font-size: var(--md-body-s-size); color: var(--text-lo); line-height: 1.5;",
                {body}
            }
            Button {
                variant: ButtonVariant::Secondary,
                "data-testid": "sidebar-empty-state-cta",
                onclick: move |_| navigate.call(cta_href.to_owned()),
                {cta_label}
            }
        }
    }
}
