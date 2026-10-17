//! The folder picker's header band: close, the folder and its machine, the
//! filter, files, and New folder actions, and the machine switcher when several
//! machines are online. Called by `browse::picker`; ports
//! `apps/web/src/components/browse/BrowseToolbar.tsx`. The switcher is the `ui`
//! dropdown menu, which places it and handles its dismissal.

use dioxus::prelude::*;
use roost_protocol::wire::Worker;

use crate::components::ui::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
};
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconButton, IconType, StatusDot, Surface,
    SurfaceRadius,
};

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
    let selected_fp = server_fp.clone();
    // The ui root reports the open state it wants; the page's toggle and close
    // callbacks stay the only owners of that state.
    let on_server_open_change = EventHandler::new(move |open: bool| {
        if open {
            on_toggle_server_menu.call(());
        } else {
            on_close_server_menu.call(());
        }
    });
    rsx! {
        DropdownMenu {
            open: server_menu_open,
            on_open_change: on_server_open_change,
            test_id: "browse-server-menu",
            Surface {
                class: Some("flex min-w-0 items-center gap-2 px-2 py-1".to_owned()),
                level: 2,
                radius: SurfaceRadius::None,
                IconButton {
                    size: ButtonSize::IconSm,
                    "data-testid": "browse-close",
                    icon: IconType::X,
                    label: "Close",
                    title: "Close",
                    onclick: move |_| on_close.call(()),
                }
                div { class: "flex min-w-0 flex-1 flex-col",
                    span {
                        class: "truncate text-sm font-medium",
                        "data-testid": "browse-folder-name",
                        {folder_name}
                    }
                    span {
                        class: "flex min-w-0 items-center gap-1 truncate text-xs text-muted-foreground",
                        "data-testid": "browse-machine",
                        StatusDot { status: if server_online { "ok".to_owned() } else { "idle".to_owned() } }
                        {server_label.clone()}
                    }
                }
                if online_workers.len() > 1 {
                    DropdownMenuTrigger {
                        class: "h-8 max-w-40 truncate rounded-md border-0 bg-transparent px-2 hover:bg-foreground/8 hover:text-foreground",
                        test_id: "browse-server",
                        span { class: "sr-only", {server_label.clone()} }
                        Icon { icon: IconType::ChevronsUpDown, class: "size-3" }
                    }
                }
                IconButton {
                    class: Some("data-[active=true]:bg-accent".to_owned()),
                    size: ButtonSize::IconSm,
                    "data-testid": "browse-filter-toggle",
                    icon: IconType::Search,
                    label: "Filter this folder",
                    title: "Filter this folder",
                    "data-active": filter_open.then_some("true"),
                    "aria-pressed": filter_open.to_string(),
                    onclick: move |_| on_toggle_filter.call(()),
                }
                IconButton {
                    class: Some("data-[active=true]:bg-accent".to_owned()),
                    size: ButtonSize::IconSm,
                    "data-testid": "browse-show-files",
                    icon: IconType::FileText,
                    label: "Show files in this folder",
                    title: "Show files in this folder",
                    "data-active": show_files.then_some("true"),
                    "aria-pressed": show_files.to_string(),
                    onclick: move |_| on_toggle_show_files.call(()),
                }
                Button {
                    variant: ButtonVariant::Secondary,
                    size: ButtonSize::Sm,
                    icon: Some(IconType::FolderPlus),
                    "data-testid": "browse-new",
                    title: "New folder",
                    disabled: !ready,
                    onclick: move |_| on_new_folder.call(()),
                    "New folder"
                }
            }
            DropdownMenuContent {
                class: "w-auto min-w-48 max-h-[60vh] flex flex-col gap-0.5 overflow-y-auto",
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

/// One machine in the switcher: its reachability dot, its label, and a check
/// when it is the one being browsed.
#[component]
fn ServerOption(worker: Worker, selected: bool, on_select: EventHandler<String>) -> Element {
    let worker_fp = worker.fp.as_str().to_owned();
    let label = worker.label.clone();
    rsx! {
        DropdownMenuItem {
            class: "flex w-full items-center gap-2",
            test_id: "browse-server-option",
            title: label.clone(),
            onclick: move |_| on_select.call(worker_fp.clone()),
            StatusDot { status: "ok".to_owned() }
            span { class: "min-w-0 flex-1 truncate text-left", {label} }
            if selected {
                Icon { icon: IconType::Check, class: "size-3" }
            }
        }
    }
}
