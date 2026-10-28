//! The Folders navigation tree, newest activity first, with stock Sidenav parts.
//! Filtering projects matching terminal and agent destinations without folder
//! headers. This component also owns the sidebar keyboard cursor's row order.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::Store;
use roost_client_core::store::navigation::query::normalize_navigation_search_query;
use roost_client_core::store::pending_close::is_pending_close;
use roost_client_core::store::selectors::{session_by_id, session_folder_key};
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::sidebar::folder_group_filter::filter_folder_groups_for_store;
use roost_client_core::store::sidebar::folder_groups::{
    FolderGroup, build_folder_groups, filter_folder_groups,
};
use roost_protocol::wire::{Session, SessionKind};

use super::folder_conversations::FolderConversationRows;
use super::folder_row::FolderRow;
use super::session_row::SessionRow;
use crate::components::ui::empty::EmptyState;
use crate::components::ui::{
    IconType, SidenavContent, SidenavGroup, SidenavGroupContent, SidenavGroupLabel, SidenavMenu,
    SidenavMenuSub,
};
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
    let compact = crate::components::layout::window_size::use_is_compact();
    let path = use_location();
    let core = pump.core();
    let (
        groups,
        filtered_ids,
        active_folder_key,
        cursor_id,
        active_agent_id,
        visible_session_ids_by_folder,
    ) = {
        let core = core.borrow();
        let store = core.store();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let all = build_folder_groups(store, &BrowserWorkerPaths, now_ms);
        let visible: Vec<FolderGroup> = all
            .into_iter()
            .filter(|group| !is_chat_folder(&group.spawn_cwd))
            .collect();
        let folder_matches = filter_folder_groups(&visible, &query);
        let folder_match_keys: Vec<&str> = folder_matches
            .iter()
            .map(|group| group.key.as_str())
            .collect();
        let groups = filter_folder_groups_for_store(store, &visible, &query);
        let visible_session_ids_by_folder = groups
            .iter()
            .map(|group| {
                let visible_session_ids = group
                    .session_ids
                    .iter()
                    .filter(|session_id| is_visible_space_session(store, session_id))
                    .cloned()
                    .collect::<Vec<_>>();
                (group.key.clone(), visible_session_ids)
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let filtered_ids: Vec<String> = if has_active_filter(&query) {
            groups
                .iter()
                .filter(|group| folder_match_keys.contains(&group.key.as_str()))
                .flat_map(|group| group.session_ids.iter())
                .filter(|id| is_visible_space_session(store, id))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let active_folder_key = active_session_for_path(store, &BrowserWorkerPaths, &path.read())
            .map(|session| session_folder_key(store, &BrowserWorkerPaths, session));
        let active_agent_id = match crate::routes::Route::parse(&path.read()) {
            crate::routes::Route::Agent { conversation_id } => Some(conversation_id),
            _ => None,
        };
        let cursor_id = store.sidebar.cursor.cursor_session_id().map(str::to_owned);
        (
            groups,
            filtered_ids,
            active_folder_key,
            cursor_id,
            active_agent_id,
            visible_session_ids_by_folder,
        )
    };
    let filtering = has_active_filter(&query);
    let visible_ids: Vec<String> = if filtering {
        filtered_ids.clone()
    } else {
        if compact {
            groups
                .iter()
                .flat_map(|group| {
                    visible_session_ids_by_folder
                        .get(&group.key)
                        .into_iter()
                        .flatten()
                        .cloned()
                })
                .collect()
        } else {
            groups
                .iter()
                .filter(|group| !group.lead_id.is_empty())
                .map(|group| group.lead_id.clone())
                .collect()
        }
    };
    let has_filtered_agents = groups
        .iter()
        .any(|group| !group.agent_conversation_ids.is_empty());
    publish_cursor_targets(&pump, active, visible_ids);

    rsx! {
            SidenavContent { "data-testid": "folder-list",
                SidenavGroup {
                    SidenavGroupLabel { if filtering { "Matches" } else { "Folders" } }
                    SidenavGroupContent {
                if !filtering {
                    if groups.is_empty() {
                        EmptyState {
                            icon: IconType::FolderX,
                            title: "No folders yet",
                            supporting: "Open a terminal to add your first folder.",
                        }
                    } else {
                        SidenavMenu {
                            for group in groups {
                                FolderRow {
                                    key: "{group.key}",
                                    selected: active_folder_key.as_deref() == Some(group.key.as_str())
                                        || active_agent_id.as_ref().is_some_and(|id| group.agent_conversation_ids.contains(id)),
                                    cursor: !compact
                                        && !group.lead_id.is_empty()
                                        && cursor_id.as_deref() == Some(group.lead_id.as_str()),
                                    group: group.clone(),
                                    SidenavMenuSub {
                                        FolderConversationRows {
                                            conversation_ids: group.agent_conversation_ids.clone(),
                                            active_agent_id: active_agent_id.clone(),
                                            nested: false,
                                            search_result: false,
                                            compact,
                                            query: String::new(),
                                        }
                                        if compact {
                                            for session_id in visible_session_ids_by_folder
                                                .get(&group.key)
                                                .into_iter()
                                                .flatten()
                                            {
                                                SessionRow {
                                                    key: "{session_id}",
                                                    cursor: cursor_id.as_deref()
                                                        == Some(session_id.as_str()),
                                                    session_id: session_id.clone(),
                                                    query: String::new(),
                                                    disambiguator: session_row_disambiguator(
                                                        &pump,
                                                        &group.session_ids,
                                                        session_id,
                                                    ),
                                                }
                                            }
                                        }
                                    }
                            }
                        }
                    }
                    }
                } else if filtered_ids.is_empty() && !has_filtered_agents {
                    EmptyState {
                        icon: IconType::SearchX,
                        title: "No matches",
                        supporting: format!("Nothing matches \"{query}\". Esc clears the search."),
                    }
                } else {
                    SidenavMenu {
                        for group in groups {
                            div { class: "flex min-w-0 flex-col gap-1",
                                SidenavGroupLabel {
                                    class: "flex min-w-0 items-center justify-between gap-2",
                                    {folder_group_heading(&group.name, &group.server)}
                                }
                                FolderConversationRows {
                                    conversation_ids: group.agent_conversation_ids,
                                    active_agent_id: active_agent_id.clone(),
                                    nested: false,
                                    search_result: true,
                                    compact,
                                    query: query.clone(),
                                }
                                for session_id in group.session_ids.iter().filter(|id| filtered_ids.contains(id)) {
                                    SessionRow {
                                        key: "{session_id}",
                                        cursor: cursor_id.as_deref() == Some(session_id.as_str()),
                                        session_id: session_id.clone(),
                                        query: query.clone(),
                                        disambiguator: if compact {
                                            session_row_disambiguator(
                                                &pump,
                                                &group.session_ids,
                                                session_id,
                                            )
                                        } else {
                                            None
                                        },
                                    }
                                }
                            }
                        }
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
        effect_pump.dispatch(ClientEvent::Sidebar(SidebarIntent::PublishCursorTargets(
            targets,
        )));
    }));
    let drop_pump = pump.clone();
    use_drop(move || {
        drop_pump.dispatch(ClientEvent::Sidebar(SidebarIntent::PublishCursorTargets(
            Vec::new(),
        )));
    });
}

fn folder_group_heading(name: &str, machine: &str) -> String {
    format!("{name} · {machine}")
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

fn session_row_disambiguator(
    pump: &Pump,
    group_session_ids: &[String],
    session_id: &str,
) -> Option<String> {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let session = session_by_id(store, session_id)?;
    let title = crate::session_naming::session_title(store, session);
    let mut matching_ids = group_session_ids.iter().filter(|candidate_id| {
        session_by_id(store, candidate_id).is_some_and(|candidate| {
            crate::session_naming::session_title(store, candidate) == title
        })
    });
    let position = matching_ids.position(|candidate_id| candidate_id == session_id)?;
    if matching_ids.next().is_some() || position > 0 {
        Some(format!("#{}", position + 1))
    } else {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::folder_group_heading;

    #[test]
    fn folder_search_heading_names_the_folder_and_machine_once() {
        assert_eq!(folder_group_heading("roost", "m1"), "roost · m1");
    }
}
