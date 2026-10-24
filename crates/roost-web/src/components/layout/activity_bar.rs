//! The desktop activity rail: the five fixed destinations and which one the
//! current path selects; clicking the ACTIVE Workspaces item toggles the sidebar
//! instead of navigating. Terminal text controls appear only on terminal
//! routes; agent work has no terminal chrome. Mounted by `AppShell` on a
//! desktop layout.
//!
//! Every destination, its icon, label, href and active test live in
//! `shell_metrics::Destination`; this component draws the list.

use dioxus::prelude::*;
use libero::{
    components::{Box, Flex, Pictogram},
    sx::sx,
};

use super::shell_metrics::{Destination, active_destination};
use crate::components::term_font_stepper::TermFontStepper;
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
    let rail_sx = use_hook(|| {
        sx().display("flex")
            .min_height("0")
            .flex_direction("column")
            .border_right("1px solid var(--lsx-paper-border-color)")
            .background("var(--workbench-activity)")
            .with("grid-area", "activity")
    });
    let group_sx = use_hook(|| sx().display("flex").flex_direction("column"));
    let bottom_sx = use_hook(|| group_sx.clone().margin_top("auto"));
    rsx! {
        Box { component: "nav", sx: rail_sx, "aria-label": "Workbench navigation",
            Flex { direction: "column", sx: group_sx,
                for destination in top {
                    ActivityItem { destination, active, on_toggle_sidebar }
                }
            }
            Flex { direction: "column", sx: bottom_sx,
                if terminal_route {
                    TermFontStepper {
                        test_id: "rail-term-font",
                        size: "sm",
                        vertical: true,
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
    let item_sx = use_hook(|| {
        let selected = sx().background("primary.1").color("ink").selector(
            "&::before",
            sx().position("absolute")
                .top("sm")
                .bottom("sm")
                .left("0")
                .width("var(--workbench-border-width)")
                .background("primary")
                .with("content", "\"\""),
        );
        sx().position("relative")
            .display("flex")
            .height("var(--workbench-activity-width)")
            .align_items("center")
            .justify_content("center")
            .color("muted")
            .text_decoration("none")
            .hover(sx().background("neutral.2").color("ink"))
            .selector(
                "&:focus-visible",
                sx().outline("var(--lsx-focus-ring-width) solid var(--lsx-focus-ring-color)")
                    .outline_offset("-2px"),
            )
            .selector("&[data-active=true]", selected)
    });
    rsx! {
        Box {
            component: "a",
            sx: item_sx,
            id: (destination == Destination::Workspaces).then_some(WORKSPACES_ITEM_ID),
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
            Pictogram { icon: destination.icon(), width: "1rem", height: "1rem" }
        }
    }
}
