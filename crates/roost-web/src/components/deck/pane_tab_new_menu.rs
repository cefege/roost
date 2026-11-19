//! A pane strip's new-terminal button: a terminal beside the selected one, or
//! the machine and folder picker. Creation is owned by `TerminalDeck`.

use dioxus::prelude::*;

use crate::components::ui::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger, Icon, IconType,
};
use crate::router_state::use_navigate;
use crate::routes::Route;

#[component]
pub fn PaneTabNewMenu(on_new_terminal: EventHandler<()>) -> Element {
    let mut open = use_signal(|| false);
    let navigate = use_navigate();
    rsx! {
        DropdownMenu {
            open: open(),
            on_open_change: move |next: bool| open.set(next),
            test_id: "tab-new-menu",
            DropdownMenuTrigger {
                class: "size-[var(--workbench-tab-action-size)] p-0 shrink-0",
                ghost: true,
                test_id: "tab-new",
                aria_label: "New tab",
                Icon { icon: IconType::Plus, class: "size-4" }
            }
            DropdownMenuContent { class: "w-52",
                DropdownMenuItem {
                    test_id: "tab-new-terminal",
                    onclick: move |_| {
                        open.set(false);
                        on_new_terminal.call(());
                    },
                    NewTabRow { icon: IconType::Terminal, label: "New terminal here" }
                }
                DropdownMenuItem {
                    test_id: "tab-new-pick-machine",
                    onclick: move |_| {
                        open.set(false);
                        navigate.call(Route::Browse { worker_fp: None }.to_path());
                    },
                    NewTabRow { icon: IconType::Monitor, label: "Choose machine…" }
                }
            }
        }
    }
}

#[component]
fn NewTabRow(icon: IconType, label: &'static str) -> Element {
    rsx! {
        span { class: "flex items-center gap-2",
            Icon { icon, class: "size-4 shrink-0 text-muted-foreground" }
            span { "{label}" }
        }
    }
}
