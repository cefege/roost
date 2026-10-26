//! The Recent view: agent conversations, newest first, grouped Today and
//! Earlier. Each row is the shared `AgentConversationRow`. Terminal sessions
//! stay in the Folders view. Reads the store's conversation table directly.

use dioxus::prelude::*;

use super::agent_conversation_row::AgentConversationRow;
use super::rel_time_tick::use_rel_time_now;
use crate::components::ui::sidenav_layout::{SidenavGroup, SidenavGroupContent, SidenavGroupLabel};
use crate::components::ui::{EmptyState, IconType, SidenavMenu, SidenavMenuItem};
use crate::pump::use_store;
use crate::router_state::use_location;
use crate::routes::Route;

const DAY_MS: i64 = 86_400_000;

/// The conversation list, filtered by the sidebar's search text.
#[component]
pub fn RecentConversations(query: String) -> Element {
    let pump = use_store();
    let now_ms = use_rel_time_now();
    let location = use_location();
    let needle = query.trim().to_lowercase();
    let mut rows: Vec<(String, i64)> = {
        let core = pump.core();
        let core = core.borrow();
        core.store()
            .agent_chat
            .conversations
            .values()
            .filter(|conversation| {
                needle.is_empty() || conversation.title.to_lowercase().contains(&needle)
            })
            .map(|conversation| (conversation.id.clone(), conversation.updated_ms))
            .collect()
    };
    rows.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let (today, earlier): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .partition(|(_, updated_ms)| now_ms - *updated_ms < DAY_MS);
    let selected_id = match Route::parse(&location.read()) {
        Route::Agent { conversation_id } => Some(conversation_id),
        _ => None,
    };
    let groups = [("Today", ids(today)), ("Earlier", ids(earlier))];
    let empty = groups.iter().all(|(_, group)| group.is_empty());
    rsx! {
        div { class: "flex min-h-0 flex-1 flex-col overflow-y-auto px-2 pb-2", "data-testid": "sidebar-recent",
            if empty {
                EmptyState {
                    icon: if needle.is_empty() { IconType::Bot } else { IconType::SearchX },
                    title: if needle.is_empty() { "No agent sessions yet" } else { "No matches" },
                }
            }
            for (label, group) in groups.into_iter().filter(|(_, group)| !group.is_empty()) {
                SidenavGroup { key: "{label}",
                    SidenavGroupLabel { "{label}" }
                    SidenavGroupContent {
                        SidenavMenu {
                            for conversation_id in group {
                                SidenavMenuItem { key: "{conversation_id}",
                                    AgentConversationRow {
                                        selected: selected_id.as_deref() == Some(conversation_id.as_str()),
                                        conversation_id,
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

fn ids(rows: Vec<(String, i64)>) -> Vec<String> {
    rows.into_iter()
        .map(|(conversation_id, _)| conversation_id)
        .collect()
}
