//! The picker's markup: the toolbar band, the path band, the entry region (or
//! the machine-unavailable panel), the launch bar and the new-folder dialog.
//! Every value arrives computed and every control reports back; the page owns
//! the state and the store owns the directory. Called by `browse::picker::page`;
//! ports `apps/web/src/components/browse/WorkerBrowsePage.tsx`.

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
use crate::components::ui::{Button, ButtonVariant, IconType, TextField};
use crate::pump::Pump;

/// The page's content for the machine in `worker_fp`.
#[component]
pub fn PickerRegions(
    pump: Pump,
    worker_fp: String,
    reading: PickerReading,
    compact: bool,
    cursor: Signal<i64>,
    mut show_hidden: Signal<bool>,
    mut path_input: Signal<String>,
    new_folder: Signal<NewFolderForm>,
    server_menu_open: Signal<bool>,
    crumb_menu_open: Signal<bool>,
    controls: PickerControls,
) -> Element {
    let view = reading.view;
    let c = controls;
    let on_close = c.on_close;
    let folder_name = c.folder_name.clone();
    let crumb_views = c.crumb_views.clone();
    let unavailable = c.unavailable;
    let on_open_here = c.on_open_here;
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
    let on_go_home = c.on_go_home;
    let on_new_folder_name = c.on_new_folder_name;
    let on_close_new_folder = c.on_close_new_folder;
    let on_create_folder = c.on_create_folder;
    let online_workers = reading.online_workers;
    let active_idx = cursor();
    let target = super::selected_target(&view, active_idx);
    use_effect(use_reactive((&view.resolved,), move |(path,)| {
        path_input.set(path)
    }));
    let root = view
        .crumbs
        .first()
        .map(|crumb| crumb.path.clone())
        .unwrap_or_else(|| view.resolved.clone());
    let places = reading.places;
    let mut recents = view.recents;
    recents.retain(|path| !places.iter().any(|(_, saved)| saved == path));
    let recents = recents.into_iter().map(|path| {
        let label =
            crate::platform::worker_paths::worker_path_basename(view.worker_os.as_deref(), &path)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| path.clone());
        (label, path)
    });
    rsx! {
        div {
            class: "flex h-full min-h-0 flex-col overflow-hidden",
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
            div { class: "px-2 pb-1",
                TextField {
                    id: Some("browse-path-input".to_owned()),
                    value: path_input(),
                    aria_label: Some("Folder path".to_owned()),
                    test_id: Some("browse-path-input".to_owned()),
                    on_input: move |value: String| path_input.set(value),
                    onkeydown: move |event: KeyboardEvent| {
                        if event.key() == Key::Named(NamedKey::Enter) {
                            event.prevent_default();
                            let path = path_input.peek().trim().to_owned();
                            if !path.is_empty() { on_navigate.call(path); }
                        }
                    },
                }
            }
            div { class: "flex min-h-0 flex-1 flex-col md:flex-row",
                div { class: "flex shrink-0 gap-1 overflow-auto border-b border-border p-2 md:w-48 md:flex-col md:border-r md:border-b-0", "data-testid": "browse-places",
                    Button { variant: ButtonVariant::Ghost, onclick: move |_| on_home.call(()), "Home" }
                    Button { variant: ButtonVariant::Ghost, onclick: move |_| on_navigate.call(root.clone()), "Computer" }
                    for (name, path) in places {
                        Button { key: "{path}", variant: ButtonVariant::Ghost, title: path.clone(), onclick: move |_| on_navigate.call(path.clone()), "{name}" }
                    }
                    for (label, path) in recents {
                        Button { key: "recent-{path}", variant: ButtonVariant::Ghost, title: path.clone(), onclick: move |_| on_navigate.call(path.clone()), "{label}" }
                    }
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
                    active_idx,
                    terminal_counts: view.terminal_counts.clone(),
                    error_message: view.error_message.clone(),
                    on_select: move |index| { let mut cursor = cursor; cursor.set(index); },
                    on_drill,
                    on_clear_filter: on_close_filter,
                    on_retry,
                }
            }
            }
            div { class: "flex flex-wrap items-center justify-end gap-2 border-t border-border px-4 py-3",
                span { class: "mr-auto min-w-0 truncate text-sm text-muted-foreground", "data-testid": "browse-target", title: target.clone(), "{target}" }
                Button {
                    variant: ButtonVariant::Ghost,
                    "data-testid": "browse-hidden-toggle",
                    "aria-pressed": if show_hidden() { "true" } else { "false" },
                    onclick: move |_| show_hidden.toggle(),
                    if show_hidden() { "Hide hidden" } else { "Show hidden" }
                }
                Button { variant: ButtonVariant::Ghost, onclick: move |_| on_close.call(()), "Cancel" }
                Button {
                    variant: ButtonVariant::Default,
                    icon: Some(IconType::Terminal),
                    "data-testid": "browse-open",
                    disabled: !view.scoped,
                    onclick: move |_| on_open_here.call(()),
                    "Open terminal"
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
