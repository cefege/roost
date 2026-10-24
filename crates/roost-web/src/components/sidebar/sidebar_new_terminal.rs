//! The sidebar's pinned launch bar: its target machine is selectable when
//! several are online, while New agent and New terminal use the active
//! workspace when available and otherwise open the target machine's browser.
//! The target rule is `new_terminal_target`.

use dioxus::prelude::*;
use libero::components::{
    ActionIcon, Button, Flex, Indicator, Menu, MenuItem, Pictogram, Text, use_menu,
};
use libero::hooks::Side;
use libero::sx::sx;

use crate::components::agent_chat::launch_agent;
use crate::components::browse::BrowseLaunchMode;
use crate::components::browse::browse_mode_href;
use crate::components::browse::listing;
use crate::new_terminal_target::{
    default_new_terminal_worker_fp, effective_new_terminal_target, online_workers_by_label,
};
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::route_session::active_session_for_path;
use crate::router_state::{use_location, use_navigate};
use crate::routes::Route;

/// The action bar.
#[component]
pub fn SidebarNewTerminal() -> Element {
    let pump = use_store();
    let path = use_location();
    let navigate = use_navigate();
    let mut selected_fp = use_signal(|| None::<String>);
    let menu = use_menu();
    let (online, target_fp, context_folder) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let workers = online_workers_by_label(store, now_ms);
        let session = active_session_for_path(store, &BrowserWorkerPaths, &path.read());
        let agent = match Route::parse(&path.read()) {
            Route::Agent { conversation_id } => {
                store.agent_chat.conversations.get(&conversation_id)
            }
            _ => None,
        };
        let contextual_worker = session
            .map(|session| session.worker_fp.to_string())
            .or_else(|| agent.map(|conversation| conversation.worker_fp.clone()));
        let context_folder = session
            .map(|session| (session.worker_fp.to_string(), session.cwd.clone()))
            .or_else(|| {
                agent.map(|conversation| (conversation.worker_fp.clone(), conversation.cwd.clone()))
            })
            .filter(|(worker_fp, _)| workers.iter().any(|worker| worker.fp.as_str() == worker_fp));
        let default = default_new_terminal_worker_fp(store, now_ms, contextual_worker.as_deref());
        let target =
            effective_new_terminal_target(selected_fp.read().as_deref(), &workers, default);
        let online: Vec<(String, String)> = workers
            .iter()
            .map(|worker| (worker.fp.to_string(), worker.label.clone()))
            .collect();
        let context_folder =
            context_folder.filter(|(worker_fp, _)| target.as_deref() == Some(worker_fp.as_str()));
        (online, target, context_folder)
    };
    let target_label = target_fp.as_ref().map_or_else(String::new, |fp| {
        online
            .iter()
            .find(|(online_fp, _)| online_fp == fp)
            .map_or_else(|| fp.chars().take(8).collect(), |(_, label)| label.clone())
    });
    let selected_gone = selected_fp
        .peek()
        .as_ref()
        .is_some_and(|fp| !online.iter().any(|(online_fp, _)| online_fp == fp));
    if selected_gone {
        selected_fp.set(None);
    }
    if online.len() < 2 && menu.is_open() {
        menu.close();
    }
    let menu_items = online.iter().cloned().map(|(fp, label)| {
        MenuItem::new(label.clone()).onselect({
            let from = target_fp.clone();
            move |_| {
                tracing::info!(target: "sidebar", from = ?from, to = %fp, "new terminal target changed");
                selected_fp.set(Some(fp.clone()));
            }
        }).into()
    }).collect();
    let trigger_sx = use_hook(|| {
        sx().min_width("0")
            .flex_grow(1)
            .justify_content("flex-start")
            .gap("sm")
    });
    let row_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("sm")
            .padding("sm")
    });
    let agent_target = target_fp.clone();
    let terminal_target = target_fp.clone();
    let agent_pump = pump.clone();
    let agent_navigate = navigate;
    let agent_context = context_folder.clone();
    let terminal_pump = pump.clone();
    let terminal_navigate = navigate;
    let terminal_context = context_folder.clone();
    rsx! {
        Flex { sx: row_sx, "data-testid": "sidebar-new-terminal",
            if online.len() > 1 {
                Menu { state: menu, items: menu_items, side: Side::Top,
                    Button {
                        variant: "outlined",
                        size: "sm",
                        sx: trigger_sx,
                        attributes: menu.a11y_attributes(),
                        title: target_label.clone(),
                        "data-testid": "sidebar-new-terminal-machine",
                        Indicator { color: "success", size: "xs" }
                        Text { size: "sm", "{target_label}" }
                        Pictogram { icon: pictogram_icons_lucide::chevron_down::outlined, width: "1rem", height: "1rem" }
                    }
                }
            } else if !target_label.is_empty() {
                Text { size: "sm", "data-testid": "sidebar-new-terminal-machine-label",
                    Indicator { color: "success", size: "xs" }
                    "{target_label}"
                }
            }
            Button {
                variant: "filled",
                size: "sm",
                disabled: target_fp.is_none(),
                "data-testid": "sidebar-new-agent-button",
                title: "New agent in this workspace",
                onclick: move |_| {
                    if let Some((worker_fp, folder)) = agent_context.clone() {
                        launch_agent(agent_pump.clone(), worker_fp, folder, agent_navigate);
                    } else if let Some(worker_fp) = agent_target.clone() {
                        agent_navigate.call(browse_mode_href(&worker_fp, BrowseLaunchMode::Agent));
                    }
                },
                Pictogram { icon: pictogram_icons_lucide::bot::outlined, width: "1rem", height: "1rem" }
                "New agent"
            }
            ActionIcon {
                aria_label: "Open terminal",
                title: "Open a terminal in this workspace",
                disabled: target_fp.is_none(),
                "data-testid": "sidebar-new-terminal-button",
                onclick: move |_| {
                    if let Some((worker_fp, folder)) = terminal_context.clone() {
                        listing::launch_terminal(terminal_pump.clone(), worker_fp, folder, terminal_navigate);
                    } else if let Some(worker_fp) = terminal_target.clone() {
                        terminal_navigate.call(Route::Browse { worker_fp: Some(worker_fp) }.to_path());
                    }
                },
                Pictogram { icon: pictogram_icons_lucide::terminal::outlined, width: "1rem", height: "1rem" }
            }
        }
    }
}
