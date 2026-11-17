//! The Sessions panel: every live terminal, newest activity first, grouped by
//! the local day it last ran. Rows reuse SessionRow with a folder · machine
//! context; the keyboard cursor publishes only while this view is selected.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use roost_client_core::store::navigation::query::normalize_navigation_search_query;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::sidebar::folder_groups::{folder_display_name, server_label_for};
use roost_client_core::store::sidebar::session_groups::build_session_groups;

use super::cursor_targets::publish_cursor_targets;
use super::folder_list::folder_group_heading;
use super::session_row::SessionRow;
use super::sidebar_empty_state::{EmptyStateKind, SidebarEmptyState};
use crate::components::ui::empty::EmptyState;
use crate::components::ui::{
    IconType, SidenavContent, SidenavGroup, SidenavGroupContent, SidenavGroupLabel, SidenavMenu,
};
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;

/// The Sessions panel.
#[component]
pub fn SessionsView(active: bool, selected: bool, query: String) -> Element {
    let pump = use_store();
    let core = pump.core();
    let (groups, contexts, cursor_id, no_machines) = {
        let core = core.borrow();
        let store = core.store();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let groups = build_session_groups(
            store,
            &BrowserWorkerPaths,
            now_ms,
            local_utc_offset_minutes(),
            &query,
        );
        let contexts = groups
            .iter()
            .flat_map(|group| group.session_ids.iter())
            .filter_map(|session_id| {
                let session = session_by_id(store, session_id)?;
                let name = folder_display_name(store, &BrowserWorkerPaths, session);
                let server = server_label_for(store, session.worker_fp.as_str());
                Some((session_id.clone(), folder_group_heading(&name, &server)))
            })
            .collect::<BTreeMap<_, _>>();
        let cursor_id = store.sidebar.cursor.cursor_session_id().map(str::to_owned);
        (groups, contexts, cursor_id, store.workers.is_empty())
    };
    let visible_ids: Vec<String> = groups
        .iter()
        .flat_map(|group| group.session_ids.iter().cloned())
        .collect();
    publish_cursor_targets(
        &pump,
        selected.then(|| if active { visible_ids } else { Vec::new() }),
    );
    let filtering = !normalize_navigation_search_query(&query).is_empty();
    rsx! {
        SidenavContent { "data-testid": "session-list",
            if no_machines {
                SidebarEmptyState { kind: EmptyStateKind::NoMachines }
            } else if groups.is_empty() {
                if filtering {
                    EmptyState {
                        icon: IconType::SearchX,
                        title: "No matches",
                        supporting: format!("Nothing matches \"{query}\". Esc clears the search."),
                    }
                } else {
                    EmptyState {
                        icon: IconType::Terminal,
                        title: "No sessions yet",
                        supporting: "Open a terminal to start one.",
                    }
                }
            } else {
                for group in groups {
                    SidenavGroup {
                        SidenavGroupLabel { "{group.label}" }
                        SidenavGroupContent {
                            SidenavMenu {
                                for session_id in group.session_ids {
                                    SessionRow {
                                        key: "{session_id}",
                                        cursor: cursor_id.as_deref() == Some(session_id.as_str()),
                                        session_id: session_id.clone(),
                                        query: query.clone(),
                                        disambiguator: contexts.get(&session_id).cloned(),
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

/// The reader's offset east of UTC in minutes, so day buckets follow their
/// clock rather than the server's.
#[cfg(target_arch = "wasm32")]
fn local_utc_offset_minutes() -> i32 {
    -(js_sys::Date::new_0().get_timezone_offset() as i32)
}

#[cfg(not(target_arch = "wasm32"))]
fn local_utc_offset_minutes() -> i32 {
    0
}
