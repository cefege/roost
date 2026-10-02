//! The sidebar's pinned action bar: "New terminal" opens the folder picker on
//! the target machine, and with two or more machines online a picker menu
//! chooses that machine. Ports
//! `apps/web/src/components/sidebar/SidebarNewTerminal.tsx`; `SidebarRoot`
//! mounts it below both panels. The target rule is `new_terminal_target`.

use dioxus::prelude::*;

use crate::components::context_menu::{
    AnchoredMenuPos, CtxMenuItem, anchored_menu_pos, anchored_menu_surface_style,
    use_floating_menu_dismiss,
};
use crate::components::md::{Button, ButtonSize, ButtonVariant, Icon, IconSize, StatusDot};
use crate::new_terminal_target::{
    default_new_terminal_worker_fp, effective_new_terminal_target, online_workers_by_label,
};
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::route_session::active_session_for_path;
use crate::router_state::{use_location, use_navigate};
use crate::routes::Route;

/// The machine trigger's id.
pub const MACHINE_TRIGGER_ID: &str = "sidebar-new-terminal-machine";
/// The machine menu's id.
pub const MACHINE_MENU_ID: &str = "sidebar-new-terminal-machine-menu";

/// Where the machine menu sits: right-aligned to its trigger and opening
/// UPWARD, its bottom edge the anchor gap above the trigger's top.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MachineMenuAnchor {
    /// Offset from the viewport's right edge.
    pub right: f64,
    /// Offset from the viewport's bottom edge.
    pub bottom: f64,
}

/// The upward anchor, from the trigger's box and the viewport.
pub fn machine_menu_anchor(
    trigger_right: f64,
    trigger_top: f64,
    trigger_bottom: f64,
    viewport_width: f64,
    viewport_height: f64,
) -> MachineMenuAnchor {
    let below = anchored_menu_pos(trigger_right, trigger_bottom, viewport_width);
    MachineMenuAnchor {
        right: below.right,
        bottom: viewport_height - trigger_top + (below.y - trigger_bottom),
    }
}

/// The action bar.
#[component]
pub fn SidebarNewTerminal() -> Element {
    let pump = use_store();
    let path = use_location();
    let navigate = use_navigate();
    let mut selected_fp = use_signal(|| None::<String>);
    let mut menu_open = use_signal(|| false);
    let mut menu_anchor = use_signal(|| None::<MachineMenuAnchor>);
    let (online, target_fp) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let workers = online_workers_by_label(store, now_ms);
        let active = active_session_for_path(store, &BrowserWorkerPaths, &path.read())
            .map(|session| session.worker_fp.to_string());
        let default = default_new_terminal_worker_fp(store, now_ms, active.as_deref());
        let target =
            effective_new_terminal_target(selected_fp.read().as_deref(), &workers, default);
        let online: Vec<(String, String)> = workers
            .iter()
            .map(|worker| (worker.fp.to_string(), worker.label.clone()))
            .collect();
        (online, target)
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
    if online.len() < 2 && *menu_open.peek() {
        menu_open.set(false);
    }
    // An outside click or Escape hands focus back to the trigger, as an item
    // pick does: the reader dismissed a menu they opened from that button, and
    // focus left on `<body>` strands a keyboard reader at the top of the page.
    // Guarded on the menu being open because the listener lives as long as the
    // bar does, and every other click on the page reaches it too.
    let dismiss = move |()| {
        if !*menu_open.peek() {
            return;
        }
        menu_open.set(false);
        #[cfg(target_arch = "wasm32")]
        super::dom::focus_by_id(MACHINE_TRIGGER_ID);
    };
    use_floating_menu_dismiss(
        EventHandler::new(dismiss),
        Some(EventHandler::new(dismiss)),
        vec![MACHINE_TRIGGER_ID.to_owned(), MACHINE_MENU_ID.to_owned()],
    );
    let mut open_menu = move |edge: crate::components::context_menu::MenuFocusEdge| {
        #[cfg(target_arch = "wasm32")]
        {
            menu_anchor.set(super::dom::machine_trigger_anchor(MACHINE_TRIGGER_ID));
            menu_open.set(true);
            crate::components::context_menu::focus_menu_edge(MACHINE_MENU_ID, edge);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (edge, &mut menu_anchor);
    };
    let new_target = target_fp.clone();
    rsx! {
        footer { class: "workbench-sidebar-actionbar", "data-testid": "sidebar-new-terminal",
            Button {
                class: "workbench-sidebar-actionbar__new",
                variant: ButtonVariant::Default,
                size: ButtonSize::Sm,
                icon: "add",
                "data-testid": "sidebar-new-terminal-button",
                title: "New terminal in a folder",
                disabled: target_fp.is_none(),
                onclick: move |_| {
                    if let Some(fp) = new_target.clone() {
                        navigate.call(Route::Browse { worker_fp: Some(fp) }.to_path());
                    }
                },
                "New terminal"
            }
            if online.len() > 1 {
                Button {
                    id: MACHINE_TRIGGER_ID,
                    class: "workbench-sidebar-actionbar__machine",
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Sm,
                    "data-testid": "sidebar-new-terminal-machine",
                    title: target_label.clone(),
                    "aria-haspopup": "menu",
                    "aria-controls": MACHINE_MENU_ID,
                    "aria-expanded": if menu_open() { "true" } else { "false" },
                    onclick: move |_| {
                        if *menu_open.peek() {
                            menu_open.set(false);
                        } else {
                            open_menu(crate::components::context_menu::MenuFocusEdge::First);
                        }
                    },
                    onkeydown: move |event: KeyboardEvent| {
                        use crate::components::context_menu::MenuFocusEdge;
                        match event.key() {
                            Key::ArrowDown | Key::ArrowUp => {
                                event.prevent_default();
                                event.stop_propagation();
                                let edge = if event.key() == Key::ArrowDown { MenuFocusEdge::First } else { MenuFocusEdge::Last };
                                open_menu(edge);
                            }
                            Key::Escape if *menu_open.peek() => {
                                event.prevent_default();
                                event.stop_propagation();
                                menu_open.set(false);
                            }
                            _ => {}
                        }
                    },
                    StatusDot { status: "ok" }
                    span { class: "workbench-sidebar-actionbar__machine-label", {target_label.clone()} }
                    Icon { name: "expand_more", class: "workbench-sidebar-actionbar__machine-chevron", size: IconSize::Sm }
                }
            }
            if let (true, Some(anchor)) = (menu_open(), menu_anchor()) {
                div {
                    id: MACHINE_MENU_ID,
                    class: "df-menu-enter workbench-sidebar-actionbar__machine-menu",
                    "data-testid": "sidebar-new-terminal-machine-menu",
                    role: "menu",
                    "aria-labelledby": MACHINE_TRIGGER_ID,
                    style: anchored_menu_surface_style(
                        AnchoredMenuPos { right: anchor.right, y: 0.0 },
                        "calc(var(--control-touch-target) * 4)",
                        None,
                        &format!("top: auto; bottom: {}px;", anchor.bottom),
                    ),
                    onkeydown: move |event: KeyboardEvent| {
                        #[cfg(target_arch = "wasm32")]
                        super::dom::run_menu_key_by_id(&event, MACHINE_MENU_ID, move || {
                            menu_open.set(false);
                            super::dom::focus_by_id(MACHINE_TRIGGER_ID);
                        });
                        #[cfg(not(target_arch = "wasm32"))]
                        let _ = &event;
                    },
                    for (fp, label) in online.iter().cloned() {
                        CtxMenuItem {
                            key: "{fp}",
                            class: "workbench-sidebar-actionbar__machine-option",
                            testid: "sidebar-new-terminal-machine-option",
                            selected: target_fp.as_deref() == Some(fp.as_str()),
                            title: label.clone(),
                            onclick: {
                                let from = target_fp.clone();
                                let fp = fp.clone();
                                move |_| {
                                    tracing::info!(target: "sidebar", from = ?from, to = %fp, "new terminal target changed");
                                    selected_fp.set(Some(fp.clone()));
                                    menu_open.set(false);
                                    #[cfg(target_arch = "wasm32")]
                                    super::dom::focus_by_id(MACHINE_TRIGGER_ID);
                                }
                            },
                            StatusDot { status: "ok" }
                            span { class: "workbench-sidebar-actionbar__machine-option-label", {label.clone()} }
                            if target_fp.as_deref() == Some(fp.as_str()) {
                                Icon { name: "check", class: "workbench-sidebar-actionbar__machine-option-check", size: IconSize::Sm }
                            }
                        }
                    }
                }
            }
        }
    }
}
