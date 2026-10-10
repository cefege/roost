//! Every control the picker's page hands to a band, built once per render from
//! the store read the page already holds. Split from `picker::page` for the line
//! cap: this file is wiring, and the page beside it is lifecycle.
//!
//! Called by `browse::picker::page`. Ports the handler half of
//! `apps/web/src/components/browse/WorkerBrowsePage.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::browse_state::intent::BrowseIntent;

use crate::components::browse::dom;
use crate::components::browse::listing;
use crate::components::browse::path_bar::CrumbMenuPos;
use crate::components::browse::picker::control_set::PickerControls;
use crate::components::browse::picker::parts::{
    commit_new_folder, history_move, measure_crumb_anchor, measure_server_anchor, recents_header,
};
use crate::components::browse::toolbar::SERVER_TRIGGER_ID;
use crate::components::browse::view::PickerReading;
use crate::components::context_menu::AnchoredMenuPos;
use crate::pump::Pump;
use crate::routes::browse_href;

/// Every control for one render of the page.
#[allow(clippy::too_many_arguments)]
pub fn build_controls(
    pump: &Pump,
    worker_fp: &str,
    reading: &PickerReading,
    compact: bool,
    cursor: Signal<i64>,
    new_folder: Signal<crate::components::browse::picker::NewFolderForm>,
    ticket: Signal<u64>,
    retry: Signal<u32>,
    hide_middle: Signal<usize>,
    server_menu_open: Signal<bool>,
    server_anchor: Signal<Option<AnchoredMenuPos>>,
    crumb_menu_open: Signal<bool>,
    crumb_anchor: Signal<Option<CrumbMenuPos>>,
    navigate: EventHandler<String>,
) -> PickerControls {
    let view = &reading.view;
    let on_close = EventHandler::new(move |()| navigate.call("/".to_owned()));
    let on_toggle_filter = EventHandler::new({
        let pump = pump.clone();
        let worker_fp = worker_fp.to_owned();
        let open = view.filter_open;
        let mut ticket = ticket;
        move |()| {
            let intent = if open {
                BrowseIntent::CloseFilter {
                    worker_fp: worker_fp.clone(),
                }
            } else {
                BrowseIntent::SetFilterOpen {
                    worker_fp: worker_fp.clone(),
                    open: true,
                }
            };
            listing::apply(&pump, &mut ticket, intent);
            if !open {
                dom::focus_by_id(dom::FILTER_ID);
            }
        }
    });
    let on_toggle_show_files = EventHandler::new({
        let pump = pump.clone();
        let show = view.show_files;
        move |()| {
            let storage = crate::platform::LocalStorageKeyValueStore::new();
            pump.write_store(|store| {
                roost_client_core::store::ui::set_home_folder_show_files(store, &storage, !show);
            });
        }
    });
    let on_new_folder = EventHandler::new({
        let mut form = new_folder;
        move |()| {
            form.set(crate::components::browse::picker::NewFolderForm {
                open: true,
                name: String::new(),
                busy: false,
                error: None,
            });
            dom::focus_by_id(dom::NEW_FOLDER_ID);
        }
    });
    let on_select_server =
        EventHandler::new(move |target: String| navigate.call(browse_href(&target)));
    let on_toggle_server_menu = EventHandler::new({
        let mut open = server_menu_open;
        let mut anchor = server_anchor;
        move |()| {
            if *open.peek() {
                open.set(false);
            } else {
                anchor.set(measure_server_anchor(SERVER_TRIGGER_ID));
                open.set(true);
            }
        }
    });
    let on_toggle_crumb_menu = EventHandler::new({
        let mut open = crumb_menu_open;
        let mut anchor = crumb_anchor;
        move |()| {
            if *open.peek() {
                open.set(false);
            } else {
                anchor.set(measure_crumb_anchor(
                    crate::components::browse::path_bar::CRUMB_OVERFLOW_ID,
                ));
                open.set(true);
            }
        }
    });
    let on_drill = EventHandler::new({
        let pump = pump.clone();
        let worker_fp = worker_fp.to_owned();
        let worker_os = view.worker_os.clone();
        let base = view.cwd.clone();
        let mut cursor = cursor;
        let mut ticket = ticket;
        move |name: String| {
            let Some(path) = crate::platform::worker_paths::palette::child_path(
                worker_os.as_deref(),
                &base,
                &name,
            ) else {
                return;
            };
            listing::apply(
                &pump,
                &mut ticket,
                BrowseIntent::Navigate {
                    worker_fp: worker_fp.clone(),
                    path,
                },
            );
            cursor.set(-1);
        }
    });
    let on_up = EventHandler::new({
        let pump = pump.clone();
        let worker_fp = worker_fp.to_owned();
        let worker_os = view.worker_os.clone();
        let resolved = view.resolved.clone();
        let mut ticket = ticket;
        move |()| {
            listing::apply(
                &pump,
                &mut ticket,
                BrowseIntent::Navigate {
                    worker_fp: worker_fp.clone(),
                    path: crate::platform::worker_paths::palette::parent_path(
                        worker_os.as_deref(),
                        &resolved,
                    ),
                },
            );
        }
    });
    let on_home = EventHandler::new({
        let pump = pump.clone();
        let worker_fp = worker_fp.to_owned();
        let mut ticket = ticket;
        move |()| {
            listing::apply(
                &pump,
                &mut ticket,
                BrowseIntent::Navigate {
                    worker_fp: worker_fp.clone(),
                    path: roost_client_core::store::browse_paths::BROWSE_HOME.to_owned(),
                },
            );
        }
    });
    let on_start_agent = EventHandler::new({
        let pump = pump.clone();
        let worker_fp = worker_fp.to_owned();
        let folder = view.resolved.clone();
        move |()| {
            crate::components::agent_chat::launch_agent(
                pump.clone(),
                worker_fp.clone(),
                folder.clone(),
                navigate,
            );
        }
    });
    let on_open_here = EventHandler::new({
        let pump = pump.clone();
        let worker_fp = worker_fp.to_owned();
        let folder = view.resolved.clone();
        move |()| {
            listing::launch_terminal(pump.clone(), worker_fp.clone(), folder.clone(), navigate);
        }
    });
    let on_pick_recent = EventHandler::new({
        let pump = pump.clone();
        let worker_fp = worker_fp.to_owned();
        move |folder: String| {
            listing::launch_terminal(pump.clone(), worker_fp.clone(), folder.clone(), navigate);
        }
    });
    let on_retry = EventHandler::new({
        let mut retry = retry;
        move |()| {
            let next = retry.peek().wrapping_add(1);
            retry.set(next);
        }
    });
    let on_new_folder_name = EventHandler::new({
        let mut form = new_folder;
        move |value: String| {
            let mut next = form.peek().clone();
            next.name = value;
            next.error = None;
            form.set(next);
        }
    });
    let on_close_new_folder = EventHandler::new({
        let mut form = new_folder;
        move |()| form.set(crate::components::browse::picker::NewFolderForm::default())
    });
    let on_create_folder = EventHandler::new({
        let pump = pump.clone();
        let worker_fp = worker_fp.to_owned();
        let worker_os = view.worker_os.clone();
        let parent = view.cwd.clone();
        let siblings = reading.sibling_names.clone();
        let form = new_folder;
        let mut ticket = ticket;
        move |()| {
            commit_new_folder(
                &pump,
                &worker_fp,
                worker_os.as_deref(),
                &parent,
                &siblings,
                form,
                &mut ticket,
            );
        }
    });
    let on_navigate = EventHandler::new({
        let pump = pump.clone();
        let worker_fp = worker_fp.to_owned();
        let mut ticket = ticket;
        move |path: String| {
            listing::apply(
                &pump,
                &mut ticket,
                BrowseIntent::Navigate {
                    worker_fp: worker_fp.clone(),
                    path,
                },
            );
        }
    });
    let on_set_filter = EventHandler::new({
        let pump = pump.clone();
        let worker_fp = worker_fp.to_owned();
        let mut ticket = ticket;
        move |value: String| {
            listing::apply(
                &pump,
                &mut ticket,
                BrowseIntent::SetFilter {
                    worker_fp: worker_fp.clone(),
                    filter: value,
                },
            );
        }
    });
    let on_close_filter = EventHandler::new({
        let pump = pump.clone();
        let worker_fp = worker_fp.to_owned();
        let mut ticket = ticket;
        move |()| {
            listing::apply(
                &pump,
                &mut ticket,
                BrowseIntent::CloseFilter {
                    worker_fp: worker_fp.clone(),
                },
            );
        }
    });
    let mut back_ticket = ticket;
    let on_back = history_move(pump, worker_fp, &mut back_ticket, true);
    let mut forward_ticket = ticket;
    let on_forward = history_move(pump, worker_fp, &mut forward_ticket, false);
    let _ = compact;
    let _ = hide_middle;
    PickerControls {
        crumb_views: crate::platform::worker_paths::palette::collapse_crumbs_to(
            &view.crumbs,
            *hide_middle.peek(),
        ),
        folder_name: crate::platform::worker_paths::worker_path_basename(
            view.worker_os.as_deref(),
            &view.resolved,
        )
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| view.resolved.clone()),
        header: recents_header(view, on_pick_recent),
        unavailable: !view.scoped && view.hydrated,
        on_close,
        on_toggle_filter,
        on_toggle_show_files,
        on_new_folder,
        on_select_server,
        on_toggle_server_menu,
        on_close_server_menu: EventHandler::new({
            let mut flag = server_menu_open;
            move |()| flag.set(false)
        }),
        on_navigate,
        on_back,
        on_forward,
        on_up,
        on_home,
        on_filter: on_set_filter,
        on_close_filter,
        on_toggle_crumb_menu,
        on_close_crumb_menu: EventHandler::new({
            let mut flag = crumb_menu_open;
            move |()| flag.set(false)
        }),
        on_drill,
        on_retry,
        on_start_agent,
        on_open_here,
        on_go_home: EventHandler::new(move |()| navigate.call("/".to_owned())),
        on_new_folder_name,
        on_close_new_folder,
        on_create_folder,
    }
}
