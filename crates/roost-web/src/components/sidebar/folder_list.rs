//! The Folders panel list: one row per (machine, folder) bucket, newest
//! activity first; with an active filter, the matching buckets' terminal rows
//! instead. Owns the sidebar keyboard cursor's row order. Ports
//! `apps/web/src/components/sidebar/FolderList.tsx`; `AllView` renders it. The
//! buckets and filter are `roost_client_core::store::sidebar::folder_groups`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::navigation::query::normalize_navigation_search_query;
use roost_client_core::store::pending_close::is_pending_close;
use roost_client_core::store::selectors::{session_by_id, session_folder_key};
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::sidebar::folder_groups::{
    FolderGroup, build_folder_groups, filter_folder_groups,
};
use roost_client_core::Store;
use roost_protocol::wire::{Session, SessionKind};

use super::folder_row::FolderRow;
use super::session_row::SessionRow;
use crate::components::md::EmptyState;
use crate::platform::BrowserWorkerPaths;
use crate::pump::{Pump, use_store};
use crate::route_session::active_session_for_path;
use crate::router_state::use_location;

/// Quick-chat folders live under this segment and never show as folders.
pub const CHAT_FOLDER_SEGMENT: &str = "/.roost/chats/";

/// Whether a cwd is a quick-chat scratch folder (v2 `lib/quickChat.ts`
/// `isChatFolder`).
pub fn is_chat_folder(cwd: &str) -> bool {
    cwd.contains(CHAT_FOLDER_SEGMENT)
}

/// The list.
#[component]
pub fn FolderList(active: bool, query: String) -> Element {
    let pump = use_store();
    let path = use_location();
    let core = pump.core();
    let (groups, filtered_ids, active_folder_key, cursor_id) = {
        let core = core.borrow();
        let store = core.store();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let all = build_folder_groups(store, &BrowserWorkerPaths, now_ms);
        let visible: Vec<FolderGroup> =
            all.into_iter().filter(|group| !is_chat_folder(&group.spawn_cwd)).collect();
        let groups = filter_folder_groups(&visible, &query);
        let filtered_ids: Vec<String> = if has_active_filter(&query) {
            groups
                .iter()
                .flat_map(|group| group.session_ids.iter())
                .filter(|id| is_visible_space_session(store, id))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let active_folder_key = active_session_for_path(store, &BrowserWorkerPaths, &path.read())
            .map(|session| session_folder_key(store, &BrowserWorkerPaths, session));
        let cursor_id = store.sidebar.cursor.cursor_session_id().map(str::to_owned);
        (groups, filtered_ids, active_folder_key, cursor_id)
    };
    let filtering = has_active_filter(&query);
    let visible_ids: Vec<String> = if filtering {
        filtered_ids.clone()
    } else {
        groups.iter().map(|group| group.lead_id.clone()).collect()
    };
    publish_cursor_targets(&pump, active, visible_ids);

    rsx! {
        div { class: "workbench-sidebar-folder-list__body", "data-testid": "folder-list",
            if !filtering {
                if groups.is_empty() {
                    EmptyState {
                        icon: "folder_off",
                        title: "No folders yet",
                        supporting: "Open a terminal to add your first folder.",
                    }
                } else {
                    div { class: "df-flat-group",
                        for group in groups {
                            FolderRow {
                                key: "{group.key}",
                                selected: active_folder_key.as_deref() == Some(group.key.as_str()),
                                cursor: cursor_id.as_deref() == Some(group.lead_id.as_str()),
                                group,
                            }
                        }
                    }
                }
            } else if filtered_ids.is_empty() {
                EmptyState {
                    icon: "search_off",
                    title: "No matches",
                    supporting: format!("Nothing matches \"{query}\". Esc clears the search."),
                }
            } else {
                div { class: "df-flat-group",
                    for session_id in filtered_ids {
                        SessionRow {
                            key: "{session_id}",
                            cursor: cursor_id.as_deref() == Some(session_id.as_str()),
                            session_id,
                        }
                    }
                }
            }
        }
    }
}

/// Cursor commands target only rows in the visible Folders projection, and
/// none while the panel is inactive or unmounted.
fn publish_cursor_targets(pump: &Pump, active: bool, visible_ids: Vec<String>) {
    let targets = if active { visible_ids } else { Vec::new() };
    let effect_pump = pump.clone();
    use_effect(use_reactive((&targets,), move |(targets,)| {
        effect_pump.dispatch(ClientEvent::Sidebar(SidebarIntent::PublishCursorTargets(targets)));
    }));
    let drop_pump = pump.clone();
    use_drop(move || {
        drop_pump.dispatch(ClientEvent::Sidebar(SidebarIntent::PublishCursorTargets(Vec::new())));
    });
}

fn has_active_filter(query: &str) -> bool {
    !normalize_navigation_search_query(query).is_empty()
}

/// A filtered row: a live shell session not waiting out a close and not a
/// quick-chat scratch folder.
fn is_visible_space_session(store: &Store, session_id: &str) -> bool {
    session_by_id(store, session_id).is_some_and(|session: &Session| {
        session.kind == SessionKind::Shell
            && !is_pending_close(store, session_id)
            && !is_chat_folder(&session.cwd)
    })
}
