//! The route-stable primary sidebar: the Folders/Agents selector, shared
//! debounced filter, retained panels, compact terminal text-size row, and
//! pinned workspace launch bar. Ports
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
use crate::components::term_font_stepper::TermFontStepper;
use crate::components::ui::toggle_group::ToggleGroupSpacing;
use crate::components::ui::{ButtonSize, IconButton, IconType, ToggleGroup, ToggleGroupItem};
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
        div { class: "flex h-full min-h-0 flex-col bg-sidenav text-foreground", "data-testid": "sidebar-root",
            div {
                div { class: "flex items-center gap-2 px-2 pt-2",
                    if compact {
                        IconButton {
                            icon: IconType::X,
                            label: "Close sidebar",
                            onclick: move |_| close_drawer.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer)),
                            "data-testid": "brand-row-collapse",
                        }
                    }
                    ToggleGroup {
                        class: "min-w-0 flex-1 bg-muted p-0.5",
                        spacing: ToggleGroupSpacing::Flush,
                        aria_label: "Sidebar view",
                        test_id: "sidebar-selector",
                        ToggleGroupItem {
                            id: "sidebar-view-agents",
                            test_id: "sidebar-view-agents",
                            class: "h-8 min-w-0 flex-1 data-[state=on]:bg-background data-[state=on]:text-foreground data-[state=on]:shadow-sm",
                            title: "Agent workspaces",
                            pressed: agents_selected,
                            onclick: move |_| select_view(SidebarView::Agents),
                            "Workspaces"
                        }
                        ToggleGroupItem {
                            id: "sidebar-view-folders",
                            test_id: "sidebar-view-folders",
                            class: "h-8 min-w-0 flex-1 data-[state=on]:bg-background data-[state=on]:text-foreground data-[state=on]:shadow-sm",
                            title: "Folders",
                            pressed: folders_selected,
                            onclick: move |_| select_folders(SidebarView::Folders),
                            "Folders"
                        }
                    }
                    if compact {
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
                SidebarSearch {
                    query: query(),
                    on_change: on_query_change,
                    placeholder: if folders_selected { "Filter folders…".to_owned() } else { "Filter workspaces…".to_owned() },
                    shortcut_label: platform_shortcut_label(PlatformShortcut::SidebarSearch, "⌘F", platform).to_owned(),
                }
            }
            div { class: "relative grid min-h-0 flex-1 grid-cols-[minmax(0,1fr)] grid-rows-[minmax(0,1fr)]",
                div {
                    class: "flex min-h-0 flex-col [grid-area:1/1] data-[active=false]:invisible data-[active=false]:pointer-events-none",
                    "data-active": selected(folders_selected),
                    "data-testid": "sidebar-folders",
                    inert: (!folders_active).then_some(true),
                    "aria-hidden": (!folders_selected).then_some("true"),
                    AllView { active: folders_active, query: debounced() }
                }
                div {
                    class: "flex min-h-0 flex-col [grid-area:1/1] data-[active=false]:invisible data-[active=false]:pointer-events-none",
                    "data-active": selected(agents_selected),
                    "data-testid": "sidebar-agents-section",
                    inert: (!agents_active).then_some(true),
                    "aria-hidden": (!agents_selected).then_some("true"),
                    SidebarAgents { query: debounced() }
                }
            }
            if compact && terminal_route {
                div {
                    class: "flex items-center justify-between gap-2 border-t border-border px-3 py-2",
                    "data-testid": "sidebar-text-size",
                    span { class: "text-sm font-medium text-muted-foreground", "Text size" }
                    TermFontStepper { test_id: "sidebar-term-font", size: ButtonSize::IconSm }
                }
            }
            SidebarNewTerminal {}
        }
    }
}
