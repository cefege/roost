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
use crate::components::md::IconButton;
use crate::pump::use_pump;
use crate::router_state::use_location;

/// The bar, shown on a compact viewport.
#[component]
pub fn MobileTopBar(
    path: String,
    session_title: Option<String>,
    session_folder: Option<String>,
) -> Element {
    let pump = use_pump();
    let title = workbench_title(&path, session_title.as_deref(), session_folder.as_deref());
    rsx! {
        header { class: "mobile-topbar", "data-testid": "mobile-topbar",
            IconButton {
                class: "mobile-topbar__menu",
                icon: "menu",
                label: "Open sidebar",
                "data-testid": "mobile-topbar-menu",
                onclick: move |_| pump.dispatch(ClientEvent::Sidebar(SidebarIntent::OpenDrawer)),
            }
            span { class: "mobile-topbar__title", {title} }
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
