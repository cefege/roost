//! The desktop activity rail: the five fixed destinations and which one the
//! current path selects. Ported from
//! `apps/web/src/components/layout/WorkbenchActivityBar.tsx`.
//!
//! Every destination, its icon, its label, its href and its active test live in
//! `shell_metrics::Destination`. This component draws the list and adds nothing,
//! so a rail that disagrees with the titles about what is active is not
//! expressible.

use dioxus::prelude::*;

use super::shell_metrics::{Destination, active_destination};
use crate::components::design_icon::Icon;

/// The rail, in two groups: the destinations above the fold and the two below
/// it. The split is v2's and it is positional, not semantic — a reader learns
/// where Settings and Help live by muscle memory, so moving them would be a
/// change nobody asked for.
#[component]
pub fn ActivityBar(path: String) -> Element {
    let active = active_destination(&path);
    let top = [
        Destination::Sessions,
        Destination::Search,
        Destination::Files,
    ];
    let bottom = [Destination::Settings, Destination::Help];

    rsx! {
        nav {
            class: "workbench-activity-bar",
            "aria-label": "Workbench navigation",
            div { class: "workbench-activity-bar__group",
                for destination in top {
                    ActivityItem { destination, active }
                }
            }
            div { class: "workbench-activity-bar__group workbench-activity-bar__group--bottom",
                for destination in bottom {
                    ActivityItem { destination, active }
                }
            }
        }
    }
}

/// One rail item: the icon, the label, and the active state the stylesheet draws.
#[component]
fn ActivityItem(destination: Destination, active: Option<Destination>) -> Element {
    let is_active = active == Some(destination);
    rsx! {
        a {
            class: "workbench-activity-bar__item",
            "data-testid": destination.test_id(),
            "data-active": if is_active { "true" } else { "false" },
            href: destination.href(),
            "aria-label": destination.label(),
            "aria-current": is_active.then_some("page"),
            title: destination.label(),
            Icon { name: destination.icon(), filled: is_active }
            span { class: "workbench-activity-bar__label", {destination.label()} }
        }
    }
}
