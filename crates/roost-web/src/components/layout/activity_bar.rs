//! The desktop activity rail: the five fixed destinations and which one the
//! current path selects; clicking the ACTIVE Workspaces item toggles the sidebar
//! instead of navigating. Terminal text controls appear only on terminal
//! routes; agent work has no terminal chrome. Mounted by `AppShell` on a
//! desktop layout.
//!
//! Every destination, its icon, label, href and active test live in
//! `shell_metrics::Destination`; this component draws the list.

use dioxus::prelude::*;

use super::shell_metrics::{Destination, active_destination};
use crate::components::term_font_stepper::TermFontStepper;
use crate::components::ui::{ButtonSize, Icon};
use crate::router_state::{use_location, use_navigate};

/// The DOM id of the Workspaces item, where focus goes before a collapse.
pub const WORKSPACES_ITEM_ID: &str = "workbench-activity-workspaces";

/// Whether a click on `destination` toggles the sidebar rather than
/// navigating: only an unmodified primary click on the ACTIVE Workspaces item.
pub fn click_toggles_sidebar(
    destination: Destination,
    active: Option<Destination>,
    primary_button: bool,
    modified: bool,
) -> bool {
    destination == Destination::Workspaces
        && active == Some(destination)
        && primary_button
        && !modified
}

/// The rail, in two groups (the split is v2's and positional).
#[component]
pub fn ActivityBar(on_toggle_sidebar: EventHandler<()>) -> Element {
    let path = use_location();
    let active = active_destination(&path());
    let terminal_route = matches!(
        crate::routes::Route::parse(&path()),
        crate::routes::Route::Session { .. }
            | crate::routes::Route::Terminal { .. }
            | crate::routes::Route::Workspace { .. }
    );
    let top = [
        Destination::Workspaces,
        Destination::Search,
        Destination::Files,
    ];
    let bottom = [Destination::Settings, Destination::Help];
    rsx! {
        nav { class: "flex min-h-0 flex-col border-r border-border bg-[var(--workbench-activity)] [grid-area:activity]", "aria-label": "Workbench navigation",
            div { class: "flex flex-col",
                for destination in top {
                    ActivityItem { destination, active, on_toggle_sidebar }
                }
            }
            div { class: "mt-auto flex flex-col",
                if terminal_route {
                    TermFontStepper {
                        test_id: "rail-term-font",
                        size: ButtonSize::IconSm,
                        vertical: true,
                        class: "self-center border-b border-border py-1",
                    }
                }
                for destination in bottom {
                    ActivityItem { destination, active, on_toggle_sidebar }
                }
            }
        }
    }
}

/// One rail item.
#[component]
fn ActivityItem(
    destination: Destination,
    active: Option<Destination>,
    on_toggle_sidebar: EventHandler<()>,
) -> Element {
    let navigate = use_navigate();
    let href = destination.href();
    let is_active = active == Some(destination);
    let target = href.clone();
    rsx! {
        a {
            id: (destination == Destination::Workspaces).then_some(WORKSPACES_ITEM_ID),
            class: "relative flex h-[var(--workbench-activity-width)] items-center justify-center text-muted-foreground no-underline hover:bg-accent/50 hover:text-foreground focus-visible:outline focus-visible:outline-ring focus-visible:-outline-offset-px data-[active=true]:bg-accent data-[active=true]:text-accent-foreground data-[active=true]:before:absolute data-[active=true]:before:inset-y-2 data-[active=true]:before:left-0 data-[active=true]:before:w-px data-[active=true]:before:bg-sidenav-primary",
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
            Icon { icon: destination.icon(), class: "size-4 shrink-0" }
            span { class: "hidden", {destination.label()} }
        }
    }
}
