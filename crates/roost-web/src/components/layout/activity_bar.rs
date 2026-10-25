//! The desktop activity rail: the fixed destinations and which one the
//! current path selects. Terminal text controls appear only on terminal
//! routes; agent work has no terminal chrome. Mounted by `AppShell` on a
//! desktop layout.
//!
//! Every destination, its icon, label, href and active test live in
//! `shell_metrics::Destination`; this component draws the list.

use dioxus::prelude::*;

use super::shell_metrics::{Destination, active_destination};
use crate::components::term_font_stepper::TermFontStepper;
use crate::components::ui::{
    ButtonSize, Icon, SidenavInner, SidenavMenu, SidenavMenuButton, SidenavMenuButtonSize,
};
use crate::router_state::{use_location, use_navigate};

/// The DOM id of the Folders item, where focus goes before a collapse.
pub const FOLDERS_ITEM_ID: &str = "workbench-activity-folders";
/// Whether an unmodified primary click on the active Folders item collapses
/// the desktop sidebar rather than navigating again.
pub fn click_toggles_sidebar(
    destination: Destination,
    active: Option<Destination>,
    primary_button: bool,
    modified: bool,
) -> bool {
    destination == Destination::Folders
        && active == Some(destination)
        && primary_button
        && !modified
}

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
        Destination::Folders,
        Destination::Search,
        Destination::Files,
    ];
    let bottom = [Destination::Settings, Destination::Help];
    rsx! {
        SidenavInner { class: "min-h-0 [grid-area:activity]",
        nav { class: "flex min-h-0 flex-1 flex-col", "aria-label": "Workbench navigation",
            SidenavMenu { class: "items-center gap-1",
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
                SidenavMenu { class: "items-center gap-1",
                    for destination in bottom {
                    ActivityItem { destination, active, on_toggle_sidebar }
                    }
                }
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
    let target = href.clone();
    let is_active = active == Some(destination);
    rsx! {
        li {
            id: (destination == Destination::Folders).then_some(FOLDERS_ITEM_ID),
            "data-testid": destination.test_id(),
            onclick: move |event: MouseEvent| {
                let modified = !event.modifiers().is_empty();
                let primary = event.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary);
                if modified || !primary {
                    return;
                }
                event.prevent_default();
                if click_toggles_sidebar(destination, active, primary, modified) {
                    on_toggle_sidebar.call(());
                } else {
                    navigate.call(target.clone());
                }
            },
            title: destination.label(),
            SidenavMenuButton {
                href: Some(href),
                size: SidenavMenuButtonSize::Default,
                class: "h-[var(--workbench-activity-width)] w-[var(--workbench-activity-width)] justify-center",
                aria_current: (if is_active { "page" } else { "false" }).to_owned(),
                Icon { icon: destination.icon() }
                span { class: "sr-only", {destination.label()} }
            }
        }
    }
}
