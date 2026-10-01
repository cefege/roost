//! The folder picker's header band: close, the folder being browsed over its
//! machine, and the actions that act on this folder — filter, show files, New
//! folder — plus the machine switcher when more than one machine is online.
//! One row at every width; the path band below owns navigation. Every value
//! arrives computed, every control reports back; the page keeps state ownership.
//!
//! Called by `browse::picker`. Ports
//! `apps/web/src/components/browse/BrowseToolbar.tsx`; the anchored menu and its
//! dismissal are the app's own `context_menu` primitives.

use dioxus::prelude::*;
use roost_protocol::wire::Worker;

use crate::components::context_menu::{
    AnchoredMenuPos, CtxMenuItem, anchored_menu_surface_style, use_floating_menu_dismiss,
};
use crate::components::md::{
    Button, ButtonSize, ButtonVariant, Icon, IconButton, IconButtonSize, IconSize, StatusDot,
    Surface, SurfaceRadius,
};

/// The machine menu's element id, so its dismissal and focus are addressable.
pub const SERVER_MENU_ID: &str = "browse-server-menu";
/// The machine trigger's element id, the menu's accessible owner.
pub const SERVER_TRIGGER_ID: &str = "browse-server-trigger";

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
    /// The switcher's anchor, measured when it opened.
    server_menu_pos: Option<AnchoredMenuPos>,
    on_close: EventHandler<()>,
    on_toggle_filter: EventHandler<()>,
    on_toggle_show_files: EventHandler<()>,
    on_new_folder: EventHandler<()>,
    on_select_server: EventHandler<String>,
    on_toggle_server_menu: EventHandler<()>,
    on_close_server_menu: EventHandler<()>,
) -> Element {
    use_floating_menu_dismiss(
        on_close_server_menu,
        None,
        vec![SERVER_TRIGGER_ID.to_owned(), SERVER_MENU_ID.to_owned()],
    );
    let selected_fp = server_fp.clone();
    rsx! {
        Surface { class: "df-browse-header", level: 2, radius: SurfaceRadius::None,
            IconButton {
                size: IconButtonSize::IconSm,
                "data-testid": "browse-close",
                icon: "close",
                label: "Close".to_owned(),
                title: "Close".to_owned(),
                onclick: move |_| on_close.call(()),
            }
            div { class: "df-browse-header-title",
                span { class: "df-browse-header-folder md-title-s", "data-testid": "browse-folder-name",
                    {folder_name}
                }
                span { class: "df-browse-header-machine md-label-s", "data-testid": "browse-machine",
                    StatusDot { status: if server_online { "ok".to_owned() } else { "idle".to_owned() } }
                    {server_label.clone()}
                }
            }
            if online_workers.len() > 1 {
                Button {
                    id: SERVER_TRIGGER_ID,
                    class: "df-browse-server",
                    size: ButtonSize::Sm,
                    "data-testid": "browse-server",
                    "aria-haspopup": "menu",
                    "aria-controls": SERVER_MENU_ID,
                    "aria-expanded": server_menu_open.to_string(),
                    title: server_label.clone(),
                    onclick: move |_| on_toggle_server_menu.call(()),
                    Icon { name: "unfold_more", size: IconSize::Sm }
                }
            }
            IconButton {
                class: "df-browse-toggle",
                size: IconButtonSize::IconSm,
                "data-testid": "browse-filter-toggle",
                icon: "search",
                label: "Filter this folder".to_owned(),
                title: "Filter this folder".to_owned(),
                "data-active": filter_open.then_some("true"),
                "aria-pressed": filter_open.to_string(),
                onclick: move |_| on_toggle_filter.call(()),
            }
            IconButton {
                class: "df-browse-toggle",
                size: IconButtonSize::IconSm,
                "data-testid": "browse-show-files",
                icon: "description",
                label: "Show files in this folder".to_owned(),
                title: "Show files in this folder".to_owned(),
                "data-active": show_files.then_some("true"),
                "aria-pressed": show_files.to_string(),
                onclick: move |_| on_toggle_show_files.call(()),
            }
            Button {
                class: "df-browse-new",
                variant: ButtonVariant::Secondary,
                size: ButtonSize::Sm,
                icon: Some("create_new_folder".to_owned()),
                "data-testid": "browse-new",
                title: "New folder",
                disabled: !ready,
                onclick: move |_| on_new_folder.call(()),
                "New folder"
            }
        }
        if server_menu_open {
            if let Some(position) = server_menu_pos {
                div {
                    id: SERVER_MENU_ID,
                    class: "df-menu-enter df-browse-server-menu",
                    "data-testid": "browse-server-menu",
                    role: "menu",
                    "aria-labelledby": SERVER_TRIGGER_ID,
                    style: anchored_menu_surface_style(
                        position,
                        "calc(var(--control-touch-target) * 4)",
                        None,
                        "",
                    ),
                    for worker in online_workers.iter() {
                        ServerOption {
                            worker: worker.clone(),
                            selected: worker.fp.as_str() == selected_fp,
                            on_select: on_select_server,
                        }
                    }
                }
            }
        }
    }
}

/// One machine in the switcher: its reachability dot, its label, and a check
/// when it is the one being browsed.
#[component]
fn ServerOption(worker: Worker, selected: bool, on_select: EventHandler<String>) -> Element {
    let worker_fp = worker.fp.as_str().to_owned();
    let label = worker.label.clone();
    rsx! {
        CtxMenuItem {
            testid: "browse-server-option".to_owned(),
            class: "df-browse-server-option",
            selected,
            title: label.clone(),
            onclick: move |_| on_select.call(worker_fp.clone()),
            StatusDot { status: "ok".to_owned() }
            span { class: "df-browse-server-option-label", {label} }
            if selected {
                Icon { name: "check", size: IconSize::Sm }
            }
        }
    }
}
