//! A pane strip's "+": a new terminal or agent in the pane's folder.
//! Built on the shared `DropdownMenu`, which owns anchoring, dismissal and
//! keyboard routing; the creation itself is `TerminalDeck`'s, through the two
//! callbacks.

use dioxus::prelude::*;

use crate::components::ui::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger, Icon, IconType,
};

#[component]
pub fn PaneTabNewMenu(
    on_new_terminal: EventHandler<()>,
    on_new_agent: EventHandler<()>,
) -> Element {
    let mut open = use_signal(|| false);
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
            DropdownMenuContent { class: "w-40",
                DropdownMenuItem {
                    test_id: "tab-new-terminal",
                    onclick: move |_| {
                        open.set(false);
                        on_new_terminal.call(());
                    },
                    NewTabRow { icon: IconType::Terminal, label: "New terminal" }
                }
                DropdownMenuItem {
                    test_id: "tab-new-agent",
                    onclick: move |_| {
                        open.set(false);
                        on_new_agent.call(());
                    },
                    NewTabRow { icon: IconType::Bot, label: "New agent" }
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
