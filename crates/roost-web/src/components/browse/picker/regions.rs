//! The picker's markup: the toolbar band, the path band, the entry region (or
//! the machine-unavailable panel), the launch bar and the new-folder dialog.
//! Every value arrives computed and every control reports back; the page beside
//! it owns the state and the store owns the directory.
//!
//! Called by `browse::picker::page`. Ports the composition half of
//! `apps/web/src/components/browse/WorkerBrowsePage.tsx`.

#![allow(clippy::too_many_arguments)]

use dioxus::prelude::*;

use crate::components::browse::dom;
use crate::components::browse::entry_list::BrowseEntryList;
use crate::components::browse::new_folder::NewFolderDialog;
use crate::components::browse::path_bar::BrowsePathBar;
use crate::components::browse::picker::NewFolderForm;
use crate::components::browse::picker::control_set::PickerControls;
use crate::components::browse::toolbar::BrowseToolbar;
use crate::components::browse::unavailable::MachineUnavailable;
use crate::components::browse::view::PickerReading;
use crate::components::context_menu::AnchoredMenuPos;
use crate::components::md::{Button, ButtonVariant, Surface, SurfaceRadius};
use crate::pump::Pump;

/// The page's content for the machine in `worker_fp`.
#[component]
pub fn PickerRegions(
    pump: Pump,
    worker_fp: String,
    reading: PickerReading,
    compact: bool,
    cursor: Signal<i64>,
    new_folder: Signal<NewFolderForm>,
    server_menu_open: Signal<bool>,
    server_anchor: Signal<Option<AnchoredMenuPos>>,
    crumb_menu_open: Signal<bool>,
    crumb_anchor: Signal<Option<crate::components::browse::path_bar::CrumbMenuPos>>,
    controls: PickerControls,
) -> Element {
    let view = reading.view;
    let c = controls;
    let folder_name = c.folder_name.clone();
    let crumb_views = c.crumb_views.clone();
    let unavailable = c.unavailable;
    let header = c.header.clone();
    let on_close = c.on_close;
    let on_toggle_filter = c.on_toggle_filter;
    let on_toggle_show_files = c.on_toggle_show_files;
    let on_new_folder = c.on_new_folder;
    let on_select_server = c.on_select_server;
    let on_toggle_server_menu = c.on_toggle_server_menu;
    let on_close_server_menu = c.on_close_server_menu;
    let on_navigate = c.on_navigate;
    let on_back = c.on_back;
    let on_forward = c.on_forward;
    let on_up = c.on_up;
    let on_home = c.on_home;
    let on_filter = c.on_filter;
    let on_close_filter = c.on_close_filter;
    let on_toggle_crumb_menu = c.on_toggle_crumb_menu;
    let on_close_crumb_menu = c.on_close_crumb_menu;
    let on_drill = c.on_drill;
    let on_retry = c.on_retry;
    let on_start_agent = c.on_start_agent;
    let on_open_here = c.on_open_here;
    let on_go_home = c.on_go_home;
    let on_new_folder_name = c.on_new_folder_name;
    let on_close_new_folder = c.on_close_new_folder;
    let on_create_folder = c.on_create_folder;
    let online_workers = reading.online_workers;
    rsx! {
        div {
            class: "df-browse-page",
            id: dom::SURFACE_ID,
            "data-testid": "browse-page",
            "data-compact": if compact { "true" } else { "false" },
            "data-overlay": (!compact).then_some("true"),
            BrowseToolbar {
                folder_name,
                ready: view.scoped,
                show_files: view.show_files,
                filter_open: view.filter_open,
                server_fp: view.worker_fp.clone(),
                server_label: view.server_label.clone(),
                server_online: view.server_online,
                online_workers,
                server_menu_open: *server_menu_open.peek(),
                server_menu_pos: *server_anchor.peek(),
                on_close,
                on_toggle_filter,
                on_toggle_show_files,
                on_new_folder,
                on_select_server,
                on_toggle_server_menu,
                on_close_server_menu,
            }
            BrowsePathBar {
                crumb_views,
                crumbs: view.crumbs.clone(),
                menu_open: *crumb_menu_open.peek(),
                menu_pos: *crumb_anchor.peek(),
                back_enabled: view.can_back,
                forward_enabled: view.can_forward,
                up_enabled: view.can_up,
                filter_open: view.filter_open,
                filter: view.filter.clone(),
                on_navigate,
                on_back,
                on_forward,
                on_up,
                on_home,
                on_filter,
                on_close_filter,
                on_toggle_menu: on_toggle_crumb_menu,
                on_close_menu: on_close_crumb_menu,
            }
            if unavailable {
                MachineUnavailable {
                    on_go_home,
                }
            } else {
                BrowseEntryList {
                    status: view.status,
                    loading_caption: crate::components::browse::picker::loading_caption(view.status, view.hydrated).to_owned(),
                    folders: view.folders.clone(),
                    files: view.files.clone(),
                    show_files: view.show_files,
                    filter: view.filter.clone(),
                    server_fp: view.worker_fp.clone(),
                    worker_os: view.worker_os.clone(),
                    cwd: view.resolved.clone(),
                    now_ms: reading.now_ms,
                    active_idx: *cursor.peek(),
                    terminal_counts: view.terminal_counts.clone(),
                    error_message: view.error_message.clone(),
                    header,
                    on_drill,
                    on_clear_filter: on_close_filter,
                    on_retry,
                }
            }
            Surface { class: "df-browse-actions", level: 1, radius: SurfaceRadius::None,
                Button {
                    class: "df-browse-open",
                    variant: ButtonVariant::Default,
                    icon: Some("smart_toy".to_owned()),
                    "data-testid": "browse-start-agent",
                    disabled: !view.scoped,
                    onclick: move |_| on_start_agent.call(()),
                    "Start agent here"
                }
                Button {
                    class: "df-browse-open",
                    variant: ButtonVariant::Outline,
                    icon: Some("terminal".to_owned()),
                    "data-testid": "browse-open",
                    disabled: !view.scoped,
                    onclick: move |_| on_open_here.call(()),
                    "Open terminal here"
                }
            }
            NewFolderDialog {
                open: new_folder().open,
                name: new_folder().name,
                busy: new_folder().busy,
                error: new_folder().error,
                target_path: view.resolved.clone(),
                compact,
                on_name: on_new_folder_name,
                on_close: on_close_new_folder,
                on_create: on_create_folder,
                }
        }
    }
}
