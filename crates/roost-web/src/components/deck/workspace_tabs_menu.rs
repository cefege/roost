//! The phone terminal grid's overflow menu: close all / select tabs, or in
//! selection mode select all / close selected. `MobileDeckBar`'s sheet
//! supplies the actions; `ui::DropdownMenu` owns open state, focus, keys and
//! dismissal. Ports `apps/web/src/components/deck/WorkspaceTabsMenu.tsx`.

use dioxus::prelude::*;

use crate::components::ui::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger, Icon, IconType,
};

/// The ⋮ trigger and its menu.
#[component]
pub fn WorkspaceTabsMenu(
    selection_mode: bool,
    on_close_all: EventHandler<()>,
    on_select_tabs: EventHandler<()>,
    on_select_all: EventHandler<()>,
    on_close_selected: EventHandler<()>,
) -> Element {
    let mut open = use_signal(|| false);
    rsx! {
        DropdownMenu {
            open: open(),
            on_open_change: move |next: bool| open.set(next),
            test_id: "workspace-tabs-menu-popup",
            DropdownMenuTrigger {
                ghost: true,
                test_id: "workspace-tabs-menu",
                class: "h-9 w-9 px-0",
                Icon { icon: IconType::EllipsisVertical, class: "size-4" }
                span { class: "sr-only", "More options" }
            }
            DropdownMenuContent { class: "z-[70] w-72",
                if selection_mode {
                    DropdownMenuItem {
                        test_id: "workspace-tabs-select-all",
                        onclick: move |_| on_select_all.call(()),
                        "Select all"
                    }
                    DropdownMenuItem {
                        test_id: "workspace-tabs-close-selected",
                        class: "text-destructive",
                        onclick: move |_| on_close_selected.call(()),
                        "Close selected tabs"
                    }
                } else {
                    DropdownMenuItem {
                        test_id: "workspace-tabs-close-all",
                        class: "text-destructive",
                        onclick: move |_| on_close_all.call(()),
                        "Close all tabs"
                    }
                    DropdownMenuItem {
                        test_id: "workspace-tabs-select",
                        onclick: move |_| on_select_tabs.call(()),
                        "Select tabs"
                    }
                }
            }
        }
    }
}
