//! The compact top bar: the drawer control and the current route's title, shown
//! above the editor on a phone. Ported from
//! `apps/web/src/components/layout/MobileTopBar.tsx`; mounted by `AppShell` when
//! `shell_style::shows_mobile_top_bar` says so, and by the deck's compact chrome
//! (`MobileRouteTopBar`) on a terminal route with no session to put a bar over.
//!
//! A bar in the editor's flex column, not a fixed overlay: a floating bar would
//! occlude the input line.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;

use super::shell_metrics::workbench_title;
use crate::pump::use_pump;
use crate::router_state::use_location;
use libero::{
    components::{ActionIcon, Box, Pictogram, Text},
    sx::sx,
};

/// The bar, shown on a compact viewport.
#[component]
pub fn MobileTopBar(
    path: String,
    session_title: Option<String>,
    session_folder: Option<String>,
) -> Element {
    let pump = use_pump();
    let title = workbench_title(&path, session_title.as_deref(), session_folder.as_deref());
    let bar_sx = use_hook(|| {
        sx().display("flex")
            .height("var(--workbench-activity-width)")
            .flex_shrink("0")
            .align_items("center")
            .gap("sm")
            .border_bottom("1px solid var(--lsx-paper-border-color)")
            .background("var(--workbench-titlebar)")
            .padding_inline("sm")
            .color("ink")
    });
    let title_sx = use_hook(|| {
        sx().min_width("0")
            .flex_grow("1")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
    });
    rsx! {
        Box { component: "header", sx: bar_sx, "data-testid": "mobile-topbar",
            ActionIcon {
                aria_label: "Open sidebar",
                "data-testid": "mobile-topbar-menu",
                onclick: move |_| pump.dispatch(ClientEvent::Sidebar(SidebarIntent::OpenDrawer)),
                Pictogram { icon: pictogram_icons_lucide::menu::outlined, width: "1rem", height: "1rem" }
            }
            Text { component: "span", sx: title_sx, size: "sm", "{title}" }
        }
    }
}

/// The bar for the current route with no session behind it, so the drawer
/// keeps a focusable, non-swipe way in on a terminal route that resolved to
/// nothing.
#[component]
pub fn MobileRouteTopBar() -> Element {
    let path = use_location()();
    rsx! {
        MobileTopBar { path, session_title: None, session_folder: None }
    }
}
