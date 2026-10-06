//! The sidebar's pinned action bar: the target machine's name — with two or
//! more machines online, the trigger of a picker menu; with one, a plain
//! label — then a compact "New" that opens the folder picker on it. Ports
//! `apps/web/src/components/sidebar/SidebarNewTerminal.tsx`; `SidebarRoot`
//! mounts it below both panels. The target rule is `new_terminal_target`.

use dioxus::prelude::*;

use crate::components::context_menu::{
    AnchoredMenuPos, CtxMenuItem, MenuFocusEdge, MenuFocusRequest, anchored_menu_pos,
    anchored_menu_surface_style, attempt_menu_focus, focus_menu_edge, use_floating_menu_dismiss,
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

/// What a key pressed on the machine trigger does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineTriggerKeyAction {
    /// Open the menu with this edge focused; the event is consumed.
    Open(MenuFocusEdge),
    /// The menu is open but focus is still on the trigger: focus this edge;
    /// the event is consumed.
    FocusEdge(MenuFocusEdge),
    /// Close the menu, focus staying on the trigger; the event is consumed.
    Close,
    /// Not the trigger's key.
    Ignore,
}

/// The trigger's keys. Home/End only reach the trigger while the menu is open
/// when they beat the menu's first focus there, and they still pick their
/// edge, as they would have from inside the menu: the reader pressed them
/// after opening it, so dropping them on the render's timing loses a key.
pub fn machine_trigger_key_action(key: &str, menu_open: bool) -> MachineTriggerKeyAction {
    match (key, menu_open) {
        ("ArrowDown", _) => MachineTriggerKeyAction::Open(MenuFocusEdge::First),
        ("ArrowUp", _) => MachineTriggerKeyAction::Open(MenuFocusEdge::Last),
        ("Home", true) => MachineTriggerKeyAction::FocusEdge(MenuFocusEdge::First),
        ("End", true) => MachineTriggerKeyAction::FocusEdge(MenuFocusEdge::Last),
        ("Escape", true) => MachineTriggerKeyAction::Close,
        _ => MachineTriggerKeyAction::Ignore,
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
    let mut pending_focus = use_signal(|| None::<MenuFocusRequest>);
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
    // v2 `closeMachineMenu`: a focus request dies with its menu, so a late
    // retry never lands in a menu the reader already left.
    let mut close_menu = move || {
        if let Some(request) = pending_focus.peek().clone() {
            request.cancel();
        }
        menu_open.set(false);
    };
    if online.len() < 2 && *menu_open.peek() {
        close_menu();
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
        close_menu();
        #[cfg(target_arch = "wasm32")]
        super::dom::focus_by_id(MACHINE_TRIGGER_ID);
    };
    use_floating_menu_dismiss(
        EventHandler::new(dismiss),
        Some(EventHandler::new(dismiss)),
        vec![MACHINE_TRIGGER_ID.to_owned(), MACHINE_MENU_ID.to_owned()],
    );
    // v2 `cancelPendingFocus`: the newest request is the only one in flight.
    let mut request_focus = move |edge: MenuFocusEdge| {
        if let Some(previous) = pending_focus.peek().clone() {
            previous.cancel();
        }
        pending_focus.set(Some(focus_menu_edge(MACHINE_MENU_ID, edge)));
    };
    let mut open_menu = move |edge: MenuFocusEdge| {
        #[cfg(target_arch = "wasm32")]
        {
            menu_anchor.set(super::dom::machine_trigger_anchor(MACHINE_TRIGGER_ID));
            menu_open.set(true);
            request_focus(edge);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (edge, &mut menu_anchor);
    };
    let new_target = target_fp.clone();
    rsx! {
        footer { class: "workbench-sidebar-actionbar", "data-testid": "sidebar-new-terminal",
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
                            close_menu();
                        } else {
                            open_menu(MenuFocusEdge::First);
                        }
                    },
                    onkeydown: move |event: KeyboardEvent| {
                        let action = machine_trigger_key_action(&event.key().to_string(), *menu_open.peek());
                        if action == MachineTriggerKeyAction::Ignore {
                            return;
                        }
                        event.prevent_default();
                        event.stop_propagation();
                        match action {
                            MachineTriggerKeyAction::Open(edge) => open_menu(edge),
                            MachineTriggerKeyAction::FocusEdge(edge) => request_focus(edge),
                            MachineTriggerKeyAction::Close => close_menu(),
                            MachineTriggerKeyAction::Ignore => {}
                        }
                    },
                    StatusDot { status: "ok" }
                    span { class: "workbench-sidebar-actionbar__machine-label", {target_label.clone()} }
                    Icon { name: "expand_more", class: "workbench-sidebar-actionbar__machine-chevron", size: IconSize::Sm }
                }
            } else if !target_label.is_empty() {
                span {
                    class: "workbench-sidebar-actionbar__machine",
                    "data-testid": "sidebar-new-terminal-machine-label",
                    title: target_label.clone(),
                    StatusDot { status: "ok" }
                    span { class: "workbench-sidebar-actionbar__machine-label", {target_label.clone()} }
                }
            }
            Button {
                class: "workbench-sidebar-actionbar__new",
                variant: ButtonVariant::Default,
                size: ButtonSize::Sm,
                icon: "add",
                "data-testid": "sidebar-new-terminal-button",
                "aria-label": "New terminal",
                title: "New terminal in a folder",
                disabled: target_fp.is_none(),
                onclick: move |_| {
                    if let Some(fp) = new_target.clone() {
                        navigate.call(Route::Browse { worker_fp: Some(fp) }.to_path());
                    }
                },
                "New"
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
                    // The first focus lands in the render that inserted the
                    // menu, before the reader's next key can reach the trigger.
                    onmounted: move |_| {
                        if let Some(request) = pending_focus.peek().clone() {
                            attempt_menu_focus(&request);
                        }
                    },
                    onkeydown: move |event: KeyboardEvent| {
                        #[cfg(target_arch = "wasm32")]
                        super::dom::run_menu_key_by_id(&event, MACHINE_MENU_ID, move || {
                            close_menu();
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
                                    close_menu();
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
