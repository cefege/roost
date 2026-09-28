//! The desktop activity rail: the five fixed destinations and which one the
//! current path selects; clicking the ACTIVE Sessions item toggles the sidebar
//! instead of navigating. Ported from
//! `apps/web/src/components/layout/WorkbenchActivityBar.tsx`; mounted by
//! `AppShell` on a desktop layout.
//!
//! Every destination, its icon, label, href and active test live in
//! `shell_metrics::Destination`; this component draws the list.

use dioxus::prelude::*;

use super::shell_metrics::{Destination, active_destination};
use crate::components::md::Icon;
use crate::router_state::{use_location, use_navigate};

/// The DOM id of the Sessions item, where focus goes before a collapse.
pub const SESSIONS_ITEM_ID: &str = "workbench-activity-sessions";

/// Whether a click on `destination` toggles the sidebar rather than
/// navigating: only an unmodified primary click on the ACTIVE Sessions item.
pub fn click_toggles_sidebar(
    destination: Destination,
    active: Option<Destination>,
    primary_button: bool,
    modified: bool,
) -> bool {
    destination == Destination::Sessions && active == Some(destination) && primary_button && !modified
}

/// The rail, in two groups (the split is v2's and positional).
#[component]
pub fn ActivityBar(on_toggle_sidebar: EventHandler<()>) -> Element {
    let path = use_location();
    let active = active_destination(&path());
    let top = [Destination::Sessions, Destination::Search, Destination::Files];
    let bottom = [Destination::Settings, Destination::Help];
    rsx! {
        nav { class: "workbench-activity-bar", "aria-label": "Workbench navigation",
            div { class: "workbench-activity-bar__group",
                for destination in top {
                    ActivityItem { destination, active, on_toggle_sidebar }
                }
            }
            div { class: "workbench-activity-bar__group workbench-activity-bar__group--bottom",
                for destination in bottom {
                    ActivityItem { destination, active, on_toggle_sidebar }
                }
            }
        }
    }
}

/// One rail item.
#[component]
fn ActivityItem(destination: Destination, active: Option<Destination>, on_toggle_sidebar: EventHandler<()>) -> Element {
    let navigate = use_navigate();
    let href = destination.href();
    let is_active = active == Some(destination);
    let target = href.clone();
    rsx! {
        a {
            id: (destination == Destination::Sessions).then_some(SESSIONS_ITEM_ID),
            class: "workbench-activity-bar__item",
            "data-active": if is_active { "true" } else { "false" },
            "data-testid": destination.test_id(),
            href,
            "aria-label": destination.label(),
            "aria-current": is_active.then_some("page"),
            title: destination.label(),
            onclick: move |event: MouseEvent| {
                let modifiers = event.modifiers();
                let modified = !modifiers.is_empty();
                let primary = event.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary);
                if modified {
                    return;
                }
                event.prevent_default();
                if click_toggles_sidebar(destination, active, primary, modified) {
                    on_toggle_sidebar.call(());
                } else {
                    navigate.call(target.clone());
                }
            },
            Icon { name: destination.icon(), filled: is_active }
            span { class: "workbench-activity-bar__label", {destination.label()} }
        }
    }
}
