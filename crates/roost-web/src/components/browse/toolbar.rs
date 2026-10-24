//! The folder picker's header band: close, the folder and its machine, the
//! filter, files, and New folder actions, and the machine switcher when several
//! machines are online. Called by `browse::picker`; ports
//! `apps/web/src/components/browse/BrowseToolbar.tsx`. Libero owns switcher
//! focus and dismissal; the page owns its open state.

use dioxus::prelude::*;

use roost_protocol::wire::Worker;

use libero::components::{
    ActionIcon, Box, Button, Flex, Menu, MenuItem, Paper, Pictogram, Text, use_menu,
};
use libero::sx::sx;

/// The header band.
#[allow(clippy::too_many_arguments)]
#[component]
pub fn BrowseToolbar(
    /// Basename of the folder being browsed — the picker's own title.
    folder_name: String,
    /// The machine is in scope: mkdir and launch are reachable.
    ready: bool,
    /// Whether files are listed beside the folders.
    show_files: bool,
    /// Whether the filter box is showing.
    filter_open: bool,
    /// The machine's fingerprint.
    server_fp: String,
    /// What the machine is called.
    server_label: String,
    /// Whether an operator could reach it right now.
    server_online: bool,
    /// Every machine that is online, for the switcher.
    online_workers: Vec<Worker>,
    /// Whether the switcher is showing.
    server_menu_open: bool,
    on_close: EventHandler<()>,
    on_toggle_filter: EventHandler<()>,
    on_toggle_show_files: EventHandler<()>,
    on_new_folder: EventHandler<()>,
    on_select_server: EventHandler<String>,
    on_toggle_server_menu: EventHandler<()>,
    on_close_server_menu: EventHandler<()>,
) -> Element {
    let band_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("center")
            .gap("sm")
            .padding_inline("sm")
            .padding_block("xs")
    });
    let info_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .flex("1")
            .flex_direction("column")
    });
    let folder_sx = use_hook(|| {
        sx().overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .font_size("sm")
            .font_weight("500")
    });
    let machine_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("center")
            .gap("xs")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .font_size("xs")
            .color("muted")
    });
    let online_dot_sx = use_hook(|| {
        sx().width("0.5rem")
            .height("0.5rem")
            .border_radius("9999rem")
            .background("success")
    });
    let offline_dot_sx = use_hook(|| {
        sx().width("0.5rem")
            .height("0.5rem")
            .border_radius("9999rem")
            .background("muted")
    });
    let switcher_sx = use_hook(|| {
        sx().height("2rem")
            .max_width("10rem")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .padding_inline("sm")
    });
    let switch_icon_sx = use_hook(|| sx().width("0.75rem").height("0.75rem"));
    let active_sx = use_hook(|| sx().selector("&[data-active=true]", sx().background("primary.1")));
    let status_dot_sx = use_hook(|| {
        sx().width("0.5rem")
            .height("0.5rem")
            .border_radius("9999rem")
            .background("success")
    });
    let selected_fp = server_fp.clone();
    let menu = use_menu();
    use_effect(use_reactive((&server_menu_open,), move |(open,)| {
        if open {
            menu.open();
        } else {
            menu.close();
        }
    }));
    let mut last_menu_open = use_signal(|| menu.is_open());
    use_effect(move || {
        let is_open = menu.is_open();
        if is_open != *last_menu_open.peek() {
            last_menu_open.set(is_open);
            if is_open && !server_menu_open {
                on_toggle_server_menu.call(());
            } else if !is_open && server_menu_open {
                on_close_server_menu.call(());
            }
        }
    });
    let mut menu_items = Vec::with_capacity(online_workers.len());
    for worker in &online_workers {
        let worker_fp = worker.fp.as_str().to_owned();
        let label = worker.label.clone();
        let selected = worker_fp == selected_fp;
        let item = MenuItem::new(label.clone())
            .leading(rsx! { Box { component: "span", sx: status_dot_sx.clone() } });
        let item = if selected {
            item.trailing(rsx! { Pictogram { icon: pictogram_icons_lucide::check::outlined, width: "0.75rem", height: "0.75rem" } })
        } else {
            item
        };
        menu_items.push(
            item.onselect(move |_| {
                on_select_server.call(worker_fp.clone());
            })
            .into(),
        );
    }
    rsx! {

        Paper { shadow: "none", radius: "0", sx: band_sx,
            ActionIcon {
                size: "sm",
                "data-testid": "browse-close",
                icon: pictogram_icons_lucide::x::outlined,
                aria_label: "Close",
                onclick: move |_| on_close.call(()),
            }
            Flex { direction: "column", sx: info_sx,
                Text { sx: folder_sx, "data-testid": "browse-folder-name", {folder_name} }
                Flex { direction: "row", sx: machine_sx, "data-testid": "browse-machine",
                    Box { component: "span", sx: if server_online { online_dot_sx.clone() } else { offline_dot_sx.clone() } }
                    {server_label.clone()}
                }
            }
            if online_workers.len() > 1 {
                Menu { state: menu, items: menu_items,
                    Button {
                        variant: "standard",
                        size: "sm",
                        sx: switcher_sx,
                        attributes: menu.a11y_attributes(),
                        "data-testid": "browse-server",
                        "{server_label}"
                        Box { component: "span", sx: switch_icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::chevrons_up_down::outlined, width: "0.75rem", height: "0.75rem" } }
                    }
                }
            }
            ActionIcon {
                variant: "standard",
                sx: active_sx.clone(),
                size: "sm",
                "data-testid": "browse-filter-toggle",
                icon: pictogram_icons_lucide::search::outlined,
                aria_label: "Filter this folder",
                title: "Filter this folder",
                "data-active": filter_open.then_some("true"),
                "aria-pressed": filter_open.to_string(),
                onclick: move |_| on_toggle_filter.call(()),
            }
            ActionIcon {
                variant: "standard",
                sx: active_sx.clone(),
                size: "sm",
                "data-testid": "browse-show-files",
                icon: pictogram_icons_lucide::file_text::outlined,
                aria_label: "Show files in this folder",
                title: "Show files in this folder",
                "data-active": show_files.then_some("true"),
                "aria-pressed": show_files.to_string(),
                onclick: move |_| on_toggle_show_files.call(()),
            }
            Button {
                variant: "tonal",
                size: "sm",
                icon: rsx! { Pictogram { icon: pictogram_icons_lucide::folder_plus::outlined, width: "1rem", height: "1rem" } },
                "data-testid": "browse-new",
                title: "New folder",
                disabled: !ready,
                onclick: move |_| on_new_folder.call(()),
                "New folder"
            }
        }
    }
}
