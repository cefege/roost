//! The sidebar's terminal launcher. It asks which machine to start
//! on when more than one is online; a single online machine launches directly.
//! The active folder is reused on its own machine.

use dioxus::prelude::*;

use crate::components::browse::listing;
use crate::components::ui::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel,
};
use crate::components::ui::menu_focus::MenuFocusEdge;
use crate::components::ui::menu_overlay::{
    MenuContext, MenuSide, anchored_placement, request_open,
};
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconType, SidenavFooter, StatusDot,
};
use crate::new_terminal_target::{default_new_terminal_worker_fp, online_workers_by_label};
use crate::platform::BrowserWorkerPaths;
use crate::pump::{Pump, use_store};
use crate::route_session::active_session_for_path;
use crate::router_state::{use_location, use_navigate};
use crate::routes::Route;

/// The machine menus open UPWARD, their bottom edge above the button, so the
/// picker never covers the sidebar rows below it. `tests/sidebar_logic.rs` reads
/// this constant, so a change to the side fails that test rather than passing silently.
pub const MACHINE_MENU_SIDE: MenuSide = MenuSide::Above;

const TERMINAL_BUTTON_ID: &str = "sidebar-new-terminal-button";
const TERMINAL_MENU_ID: &str = "sidebar-new-terminal-machine-menu";

/// The launch context read from the store once per render.
#[derive(Clone, PartialEq)]
struct LaunchTargets {
    /// Online machines as `(fingerprint, label)`, in menu order.
    online: Vec<(String, String)>,
    /// The machine the current route points at, marked in the menu.
    suggested_fp: Option<String>,
    /// The active session's `(machine, folder)`.
    context_folder: Option<(String, String)>,
}

/// The footer's terminal launcher.
#[component]
pub fn SidebarNewTerminal() -> Element {
    let pump = use_store();
    let path = use_location();
    let targets = launch_targets(&pump, &path.read());
    rsx! {
        SidenavFooter { "data-testid": "sidebar-new-terminal",
            div { class: "flex items-center gap-2",
                LaunchMenu { targets }
            }
        }
    }
}

/// Reads the launch context for the route at `location`.
fn launch_targets(pump: &Pump, location: &str) -> LaunchTargets {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
    let workers = online_workers_by_label(store, now_ms);
    let session = active_session_for_path(store, &BrowserWorkerPaths, location);
    let context_folder =
        session.map(|session| (session.worker_fp.to_string(), session.cwd.clone()));
    let contextual_worker = context_folder.as_ref().map(|(fp, _)| fp.as_str());
    LaunchTargets {
        online: workers
            .iter()
            .map(|worker| (worker.fp.to_string(), worker.label.clone()))
            .collect(),
        suggested_fp: default_new_terminal_worker_fp(store, now_ms, contextual_worker),
        context_folder,
    }
}

/// One launch button and the machine menu it opens.
#[component]
fn LaunchMenu(targets: LaunchTargets) -> Element {
    let mut open = use_signal(|| false);
    rsx! {
        div { class: "flex-1",
            DropdownMenu {
                open: open(),
                on_open_change: move |next: bool| open.set(next),
                side: MACHINE_MENU_SIDE,
                trigger_id: TERMINAL_BUTTON_ID,
                menu_id: TERMINAL_MENU_ID,
                test_id: TERMINAL_MENU_ID,
                LaunchButton { targets: targets.clone() }
                DropdownMenuContent { class: "min-w-48",
                    DropdownMenuLabel { "Start on which machine?" }
                    for (fp, label) in targets.online.iter().cloned() {
                        MachineOption {
                            key: "{fp}",
                            fp: fp.clone(),
                            label,
                            suggested: targets.suggested_fp.as_deref() == Some(fp.as_str()),
                            context_folder: targets.context_folder.clone(),
                        }
                    }
                }
            }
        }
    }
}

/// The button. With several machines online it opens the menu; with one it
/// launches there; with none it is disabled.
#[component]
fn LaunchButton(targets: LaunchTargets) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let context = try_use_context::<MenuContext>();
    let only_machine = match targets.online.as_slice() {
        [(fp, _)] => Some(fp.clone()),
        _ => None,
    };
    rsx! {
        Button {
            class: "w-full",
            variant: ButtonVariant::Outline,
            size: ButtonSize::Sm,
            icon: IconType::Terminal,
            id: TERMINAL_BUTTON_ID,
            "data-testid": TERMINAL_BUTTON_ID,
            "aria-label": "Open terminal",
            "aria-haspopup": if only_machine.is_none() { "menu" } else { "false" },
            title: "Open a new terminal",
            disabled: targets.online.is_empty(),
            onclick: move |_| {
                if let Some(fp) = only_machine.clone() {
                    launch_on_machine(fp, targets.context_folder.clone(), pump.clone(), navigate);
                } else if let Some(context) = context {
                    tracing::debug!(target: "sidebar", "terminal machine picker opened");
                    request_open(
                        context,
                        Some(anchored_placement(TERMINAL_BUTTON_ID, MACHINE_MENU_SIDE)),
                        Some(MenuFocusEdge::First),
                    );
                }
            },
            "Terminal"
        }
    }
}

/// One machine row in a launch menu.
#[component]
fn MachineOption(
    fp: String,
    label: String,
    suggested: bool,
    context_folder: Option<(String, String)>,
) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let target = fp.clone();
    rsx! {
        DropdownMenuItem {
            class: "gap-2",
            test_id: "sidebar-launch-machine-option",
            title: label.clone(),
            onclick: move |_: MouseEvent| {
                launch_on_machine(target.clone(), context_folder.clone(), pump.clone(), navigate);
            },
            StatusDot { status: "ok".to_owned() }
            span { class: "min-w-0 flex-1 truncate", {label.clone()} }
            if suggested {
                Icon { icon: IconType::Check, class: "size-4 shrink-0" }
            }
        }
    }
}

/// Starts a terminal on `worker_fp`: in the active folder when it lives on that
/// machine, else through the machine's folder browser.
fn launch_on_machine(
    worker_fp: String,
    context_folder: Option<(String, String)>,
    pump: Pump,
    navigate: EventHandler<String>,
) {
    tracing::info!(target: "sidebar", worker_fp = %worker_fp, "terminal machine chosen");
    let folder = context_folder
        .filter(|(fp, _)| *fp == worker_fp)
        .map(|(_, folder)| folder);
    match folder {
        Some(folder) => {
            listing::launch_terminal(pump, worker_fp, folder, navigate);
        }
        None => {
            navigate.call(
                Route::Browse {
                    worker_fp: Some(worker_fp),
                }
                .to_path(),
            );
        }
    }
}
