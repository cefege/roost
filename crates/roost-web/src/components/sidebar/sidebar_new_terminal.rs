//! The sidebar's pinned action bar: the target machine's name — with two or
//! more machines online, the trigger of a picker menu; with one, a plain
//! label — then a compact "New" that opens the folder picker on it. Ports
//! `apps/web/src/components/sidebar/SidebarNewTerminal.tsx`; `SidebarRoot`
//! mounts it below both panels. The target rule is `new_terminal_target`.

use dioxus::prelude::*;

use crate::components::context_menu::MenuFocusEdge;
use crate::components::ui::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
};
use crate::components::ui::menu_overlay::{
    MenuContext, MenuSide, anchored_placement, request_close, request_open,
};
use crate::components::ui::{Button, ButtonSize, ButtonVariant, Icon, IconType, StatusDot};
use crate::new_terminal_target::{
    default_new_terminal_worker_fp, effective_new_terminal_target, online_workers_by_label,
};
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::route_session::active_session_for_path;
use crate::router_state::{use_location, use_navigate};
use crate::routes::Route;

/// The machine trigger's id; the ui root focuses it when the picker closes.
pub const MACHINE_TRIGGER_ID: &str = "sidebar-new-terminal-machine";
/// The machine menu's id.
pub const MACHINE_MENU_ID: &str = "sidebar-new-terminal-machine-menu";

/// The machine menu opens UPWARD, its bottom edge above the trigger, so the
/// picker never covers the sidebar rows below it. `tests/sidebar_logic.rs` reads
/// this constant, so a change to the side fails that test rather than passing silently.
pub const MACHINE_MENU_SIDE: MenuSide = MenuSide::Above;

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
    // With the trigger gone the picker has nothing to anchor to, so it closes
    // rather than floating on its own.
    if online.len() < 2 && *menu_open.peek() {
        menu_open.set(false);
    }
    let new_target = target_fp.clone();
    rsx! {
        DropdownMenu {
            open: menu_open(),
            on_open_change: move |open: bool| menu_open.set(open),
            side: MACHINE_MENU_SIDE,
            trigger_id: MACHINE_TRIGGER_ID,
            menu_id: MACHINE_MENU_ID,
            test_id: "sidebar-new-terminal-machine-menu",
            footer {
                class: "flex items-center gap-2 border-t border-border bg-sidenav p-2",
                "data-testid": "sidebar-new-terminal",
                if online.len() > 1 {
                    MachineTrigger { target_label: target_label.clone() }
                } else if !target_label.is_empty() {
                    span {
                        class: "flex min-w-0 flex-1 items-center gap-2 px-2 text-sm text-foreground",
                        "data-testid": "sidebar-new-terminal-machine-label",
                        title: target_label.clone(),
                        StatusDot { status: "ok".to_owned() }
                        span { class: "min-w-0 truncate", {target_label.clone()} }
                    }
                }
                Button {
                    class: "shrink-0",
                    variant: ButtonVariant::Default,
                    size: ButtonSize::Sm,
                    icon: IconType::Plus,
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
                DropdownMenuContent { class: "w-auto min-w-40 overflow-y-auto",
                    for (fp, label) in online.iter().cloned() {
                        DropdownMenuItem {
                            key: "{fp}",
                            class: "gap-2",
                            test_id: "sidebar-new-terminal-machine-option",
                            title: label.clone(),
                            onclick: {
                                let from = target_fp.clone();
                                let fp = fp.clone();
                                move |_: MouseEvent| {
                                    tracing::info!(target: "sidebar", from = ?from, to = %fp, "new terminal target changed");
                                    selected_fp.set(Some(fp.clone()));
                                }
                            },
                            StatusDot { status: "ok".to_owned() }
                            span { class: "min-w-0 flex-1 truncate", {label.clone()} }
                            if target_fp.as_deref() == Some(fp.as_str()) {
                                Icon { icon: IconType::Check, class: "size-4 shrink-0" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The picker's trigger. It sits inside the ui root so its keys can drive the
/// root's open state and focus edge; `on_key` consumes the keys it owns.
#[component]
fn MachineTrigger(target_label: String) -> Element {
    let context = try_use_context::<MenuContext>();
    let on_key = Callback::new(move |event: KeyboardEvent| {
        let Some(context) = context else {
            return false;
        };
        let open = context.root.peek().open;
        let action = machine_trigger_key_action(&event.key().to_string(), open);
        if action == MachineTriggerKeyAction::Ignore {
            return false;
        }
        event.prevent_default();
        event.stop_propagation();
        match action {
            MachineTriggerKeyAction::Open(edge) => request_open(
                context,
                Some(anchored_placement(MACHINE_TRIGGER_ID, MACHINE_MENU_SIDE)),
                Some(edge),
            ),
            MachineTriggerKeyAction::FocusEdge(edge) => request_open(context, None, Some(edge)),
            MachineTriggerKeyAction::Close => request_close(context, true),
            MachineTriggerKeyAction::Ignore => {}
        }
        true
    });
    rsx! {
        DropdownMenuTrigger {
            class: "min-w-0 flex-1 justify-start gap-2 px-2",
            test_id: "sidebar-new-terminal-machine",
            on_key,
            StatusDot { status: "ok".to_owned() }
            span { class: "min-w-0 truncate text-sm text-foreground", {target_label.clone()} }
            Icon { icon: IconType::ChevronDown, class: "size-4 shrink-0 text-muted-foreground" }
        }
    }
}
