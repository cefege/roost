//! The route-stable primary sidebar: the session heading with its Recent /
//! Folders switcher, debounced search, the folder tree, the compact
//! terminal text-size row, and the terminal launcher. `AppShell` mounts it once
//! in the desktop aside or the compact drawer. State changes dispatch
//! `ClientEvent::Sidebar`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;

use super::all_view::AllView;
use super::recent_conversations::RecentConversations;
use super::rel_time_tick::provide_rel_time_ticker;
use super::sidebar_heading::{SidebarHeading, SidebarView};
use super::sidebar_new_terminal::SidebarNewTerminal;
use super::sidebar_search::SidebarSearch;
use crate::components::layout::window_size::use_is_compact;
use crate::components::term_font_stepper::TermFontStepper;
use crate::components::ui::{
    ButtonSize, IconButton, IconType, SidenavFooter, SidenavHeader, SidenavInner,
};

use crate::platform::browser_platform::{PlatformShortcut, platform_shortcut_label};
use crate::pump::use_store;
use crate::router_state::use_navigate;
use crate::routes::Route;
/// How long the filter waits after the last keystroke before re-projecting.
pub const SEARCH_DEBOUNCE_MS: u32 = 120;

/// The sidebar.
#[component]
pub fn SidebarRoot() -> Element {
    let pump = use_store();
    provide_rel_time_ticker();
    let navigate = use_navigate();
    let compact = use_is_compact();
    let location = crate::router_state::use_location();
    let terminal_route = matches!(
        Route::parse(&location()),
        Route::Session { .. } | Route::Terminal { .. } | Route::Workspace { .. }
    );
    let mut query = use_signal(String::new);
    let mut view = use_signal(SidebarView::default);
    let mut debounced = use_signal(String::new);
    let mut debounce_generation = use_signal(|| 0_u64);
    let ui = pump.core().borrow().store().ui;
    // The listener outlives renders, so it reads the size class through a cell
    // every render refreshes rather than the value captured at mount.
    let compact_now = use_hook(|| std::rc::Rc::new(std::cell::Cell::new(compact)));
    compact_now.set(compact);
    #[cfg(target_arch = "wasm32")]
    {
        let pump = pump.clone();
        let compact_now = std::rc::Rc::clone(&compact_now);
        use_hook(move || {
            std::rc::Rc::new(super::dom::listen_for_search_shortcut(move || {
                pump.dispatch(ClientEvent::Sidebar(SidebarIntent::RevealForSearch {
                    compact: compact_now.get(),
                }));
            }))
        });
    }
    #[cfg(target_arch = "wasm32")]
    let platform = crate::platform::browser_platform::browser_platform();
    #[cfg(not(target_arch = "wasm32"))]
    let platform = crate::platform::browser_platform::BrowserPlatform::Other;

    let on_query_change = move |next: String| {
        query.set(next.clone());
        let generation = *debounce_generation.peek() + 1;
        debounce_generation.set(generation);
        if next.trim().is_empty() {
            debounced.set(String::new());
            return;
        }
        spawn(async move {
            #[cfg(target_arch = "wasm32")]
            super::dom::sleep_ms(SEARCH_DEBOUNCE_MS).await;
            if *debounce_generation.peek() == generation {
                debounced.set(next);
            }
        });
    };

    let interactive = if compact {
        ui.sidebar_open
    } else {
        !ui.sidebar_collapsed
    };
    let folders_active = interactive && view() == SidebarView::Folders;
    let close_drawer = pump.clone();

    rsx! {
        SidenavInner { class: "min-h-0", variant: "Floating", "data-testid": "sidebar-root",
                if compact {
                    SidenavHeader { class: "flex-row items-center justify-between",
                        IconButton {
                            icon: IconType::X,
                            label: "Close sidebar",
                            onclick: move |_| close_drawer.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer)),
                            "data-testid": "brand-row-collapse",
                        }
                        IconButton {
                            icon: IconType::Settings,
                            label: "Settings",
                            onclick: move |_| {
                                navigate.call(Route::Settings { pane: Some("devices".to_owned()) }.to_path());
                            },
                            "data-testid": "brand-row-settings",
                        }
                    }
                }
                SidebarHeading {
                    view: view(),
                    on_view_change: move |next: SidebarView| view.set(next),
                }
                SidebarSearch {
                    query: query(),
                    on_change: on_query_change,
                    placeholder: "Search sessions".to_owned(),
                    shortcut_label: platform_shortcut_label(PlatformShortcut::SidebarSearch, "⌘F", platform).to_owned(),
                }
                div {
                    class: if view() == SidebarView::Recent { "flex min-h-0 flex-1 flex-col" } else { "hidden" },
                    "data-testid": "sidebar-recent-panel",
                    inert: (!interactive || view() != SidebarView::Recent).then_some(true),
                    RecentConversations { query: debounced() }
                }
                div {
                    class: if view() == SidebarView::Folders { "flex min-h-0 flex-1 flex-col" } else { "hidden" },
                    "data-testid": "sidebar-folders",
                    inert: (!interactive || view() != SidebarView::Folders).then_some(true),
                    AllView { active: folders_active, query: debounced() }
                }
            if compact && terminal_route {
                SidenavFooter { class: "flex-row items-center justify-between",
                    div { class: "flex items-center gap-2 text-sm", "data-testid": "sidebar-text-size",
                        span { "Text size" }
                        TermFontStepper { test_id: "sidebar-term-font", size: ButtonSize::IconSm }
                    }
                }
            }
            SidebarNewTerminal {}
        }
    }
}
