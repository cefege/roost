//! The route-stable primary sidebar: the Folders/Agents selector, the shared
//! debounced filter, both retained panels (only the selected one visible and
//! interactive), the terminal text-size row and the pinned new-terminal bar. Ports
//! `apps/web/src/components/sidebar/SidebarRoot.tsx`; `AppShell` mounts it once
//! in the desktop aside or the compact drawer. State changes dispatch
//! `ClientEvent::Sidebar`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::ui::SidebarView;

use super::all_view::AllView;
use super::rel_time_tick::provide_rel_time_ticker;
use super::sidebar_agents::SidebarAgents;
use super::sidebar_new_terminal::SidebarNewTerminal;
use super::sidebar_search::SidebarSearch;
use crate::components::layout::window_size::use_is_compact;
use crate::components::md::{Button, ButtonSize, ButtonVariant, IconButton, IconButtonSize};
use crate::components::term_font_stepper::TermFontStepper;
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
    let mut query = use_signal(String::new);
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
    let folders_selected = ui.sidebar_view == SidebarView::Folders;
    let agents_selected = ui.sidebar_view == SidebarView::Agents;
    let folders_active = interactive && folders_selected;
    let agents_active = interactive && agents_selected;
    let selected = |on: bool| if on { "true" } else { "false" };
    let select_view = {
        let pump = pump.clone();
        move |view: SidebarView| pump.dispatch(ClientEvent::Sidebar(SidebarIntent::SetView(view)))
    };
    let select_folders = select_view.clone();
    let close_drawer = pump.clone();

    rsx! {
        div { class: "workbench-sidebar-root", "data-testid": "sidebar-root",
            div {
                div { class: "workbench-sidebar-header",
                    if compact {
                        IconButton {
                            icon: "close",
                            label: "Close sidebar",
                            onclick: move |_| close_drawer.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer)),
                            "data-testid": "brand-row-collapse",
                        }
                    }
                    div {
                        class: "workbench-sidebar-selector",
                        "data-testid": "sidebar-selector",
                        role: "group",
                        "aria-label": "Sidebar view",
                        Button {
                            id: "sidebar-view-folders",
                            class: "workbench-sidebar-selector__control",
                            "data-selected": selected(folders_selected),
                            "data-testid": "sidebar-view-folders",
                            size: ButtonSize::Sm,
                            variant: ButtonVariant::Ghost,
                            "aria-pressed": selected(folders_selected),
                            onclick: move |_| select_folders(SidebarView::Folders),
                            "Folders"
                        }
                        Button {
                            id: "sidebar-view-agents",
                            class: "workbench-sidebar-selector__control",
                            "data-selected": selected(agents_selected),
                            "data-testid": "sidebar-view-agents",
                            size: ButtonSize::Sm,
                            variant: ButtonVariant::Ghost,
                            "aria-pressed": selected(agents_selected),
                            onclick: move |_| select_view(SidebarView::Agents),
                            "Agents"
                        }
                    }
                    if compact {
                        IconButton {
                            icon: "settings",
                            label: "Settings",
                            onclick: move |_| {
                                navigate.call(Route::Settings { pane: Some("devices".to_owned()) }.to_path());
                            },
                            "data-testid": "brand-row-settings",
                        }
                    }
                }
                SidebarSearch {
                    query: query(),
                    on_change: on_query_change,
                    placeholder: if folders_selected { "Filter folders…".to_owned() } else { "Filter agents…".to_owned() },
                    shortcut_label: platform_shortcut_label(PlatformShortcut::SidebarSearch, "⌘F", platform).to_owned(),
                }
            }
            div { class: "workbench-sidebar-panels",
                div {
                    class: "workbench-sidebar-panel workbench-sidebar-panel--folders",
                    "data-active": selected(folders_selected),
                    "data-testid": "sidebar-folders",
                    inert: (!folders_active).then_some(true),
                    "aria-hidden": (!folders_selected).then_some("true"),
                    AllView { active: folders_active, query: debounced() }
                }
                div {
                    class: "workbench-sidebar-panel workbench-sidebar-panel--agents",
                    "data-active": selected(agents_selected),
                    "data-testid": "sidebar-agents-section",
                    inert: (!agents_active).then_some(true),
                    "aria-hidden": (!agents_selected).then_some("true"),
                    SidebarAgents { query: debounced() }
                }
            }
            div { class: "workbench-sidebar-textsize", "data-testid": "sidebar-text-size",
                span { class: "workbench-sidebar-textsize__label md-label-m", "Text size" }
                TermFontStepper { test_id: "sidebar-term-font", size: IconButtonSize::IconSm }
            }
            SidebarNewTerminal {}
        }
    }
}
