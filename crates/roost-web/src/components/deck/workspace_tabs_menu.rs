//! The phone terminal grid's overflow menu: close all / select tabs, or select
//! all / close selected. The sheet supplies actions; Libero owns menu focus.

use dioxus::prelude::*;
use libero::components::{ActionIcon, Menu, MenuItem, use_menu};

/// The ⋮ trigger and its menu.
#[component]
pub fn WorkspaceTabsMenu(
    selection_mode: bool,
    on_close_all: EventHandler<()>,
    on_select_tabs: EventHandler<()>,
    on_select_all: EventHandler<()>,
    on_close_selected: EventHandler<()>,
) -> Element {
    let menu = use_menu();
    let items = if selection_mode {
        vec![
            MenuItem::new("Select all")
                .onselect(move |_| on_select_all.call(()))
                .into(),
            MenuItem::new("Close selected tabs")
                .onselect(move |_| on_close_selected.call(()))
                .into(),
        ]
    } else {
        vec![
            MenuItem::new("Close all tabs")
                .onselect(move |_| on_close_all.call(()))
                .into(),
            MenuItem::new("Select tabs")
                .onselect(move |_| on_select_tabs.call(()))
                .into(),
        ]
    };
    rsx! {
        Menu { state: menu, items,
            ActionIcon {
                icon: pictogram_icons_lucide::ellipsis_vertical::outlined,
                aria_label: "More options",
                attributes: menu.a11y_attributes(),
                "data-testid": "workspace-tabs-menu",
            }
        }
    }
}
