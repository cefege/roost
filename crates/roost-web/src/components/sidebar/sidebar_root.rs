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
use crate::platform::browser_platform::{PlatformShortcut, platform_shortcut_label};
use crate::pump::use_store;
use crate::router_state::use_navigate;
use crate::routes::Route;
use libero::components::{ActionIcon, Flex, Pictogram, SegmentedControl};
use libero::sx::sx;

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
    let segment_sx = use_hook(|| sx().flex_grow(1));
    let root_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .height("100%")
            .min_height("0")
            .gap("0")
            .background("surface")
            .color("ink")
    });
    let header_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("sm")
            .padding_inline("sm")
            .padding_block("sm")
    });
    let panel_stack_sx = use_hook(|| {
        sx().display("grid")
            .min_height("0")
            .flex("1")
            .gap("0")
            .with("grid-template-columns", "minmax(0, 1fr)")
            .with("grid-template-rows", "minmax(0, 1fr)")
    });
    let panel_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .min_height("0")
            .gap("0")
            .with("grid-area", "1 / 1")
            .selector(
                "[data-active=false]",
                sx().visibility("hidden").pointer_events("none"),
            )
    });
    let footer_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .justify_content("space-between")
            .gap("sm")
            .border_top("1px solid")
            .border_color("muted.4")
            .padding_inline("md")
            .padding_block("sm")
    });
    let select_view = {
        let pump = pump.clone();
        move |view: SidebarView| pump.dispatch(ClientEvent::Sidebar(SidebarIntent::SetView(view)))
    };
    let close_drawer = pump.clone();
    let segment_value = if folders_selected {
        "folders"
    } else {
        "agents"
    }
    .to_owned();
    rsx! {
        Flex { direction: "column", sx: root_sx, "data-testid": "sidebar-root",
            Flex { direction: "row", sx: header_sx,
                    if compact {
                        ActionIcon {
                            aria_label: "Close sidebar",
                            onclick: move |_| close_drawer.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer)),
                            "data-testid": "brand-row-collapse",
                            Pictogram { icon: pictogram_icons_lucide::x::outlined, width: "1rem", height: "1rem" }
                        }
                    }
                    SegmentedControl {
                        sx: segment_sx,
                        options: vec!["agents".to_owned(), "folders".to_owned()],
                        option_label: |view: String| -> libero::components::OptionLabel {
                            if view == "agents" { "Workspaces".into() } else { "Folders".into() }
                        },
                        value: segment_value,
                        onchange: move |view: String| select_view(if view == "folders" { SidebarView::Folders } else { SidebarView::Agents }),
                        "aria-label": "Sidebar view",
                        "data-testid": "sidebar-selector",
                    }
                    if compact {
                        ActionIcon {
                            aria_label: "Settings",
                            onclick: move |_| {
                                navigate.call(Route::Settings { pane: Some("devices".to_owned()) }.to_path());
                            },
                            "data-testid": "brand-row-settings",
                            Pictogram { icon: pictogram_icons_lucide::settings::outlined, width: "1rem", height: "1rem" }
                        }
                    }
                SidebarSearch {
                    query: query(),
                    on_change: on_query_change,
                    placeholder: if folders_selected { "Filter folders…".to_owned() } else { "Filter workspaces…".to_owned() },
                    shortcut_label: platform_shortcut_label(PlatformShortcut::SidebarSearch, "⌘F", platform).to_owned(),
                }
            }
            Flex { direction: "column", sx: panel_stack_sx,
                Flex { direction: "column", sx: panel_sx.clone(),
                    "data-active": selected(folders_selected),
                    "data-testid": "sidebar-folders",
                    inert: (!folders_active).then_some(true),
                    "aria-hidden": (!folders_selected).then_some("true"),
                    AllView { active: folders_active, query: debounced() }
                }
                Flex { direction: "column", sx: panel_sx,
                    "data-active": selected(agents_selected),
                    "data-testid": "sidebar-agents-section",
                    inert: (!agents_active).then_some(true),
                    "aria-hidden": (!agents_selected).then_some("true"),
                    SidebarAgents { query: debounced() }
                }
            }
            if compact && terminal_route {
                Flex { direction: "row", sx: footer_sx,
                    "data-testid": "sidebar-text-size",
                    TermFontStepper { test_id: "sidebar-term-font", size: "sm" }
                }
            }
            SidebarNewTerminal {}
        }
    }
}
