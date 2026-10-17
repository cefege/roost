//! The Agents panel: built-in agent conversations above open terminal sessions
//! with a known coding-agent status. Terminal sessions follow the Folders
//! panel's folder order and attention order; both row types link to their
//! respective surfaces.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::client::agents::status_policy::agent_status_presentation;
use roost_client_core::store::navigation::query::normalize_navigation_search_query;
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::sidebar::agents_projection::project_sidebar_agent_groups;
use roost_client_core::store::sidebar::documents::store_navigation_documents;
use roost_client_core::store::sidebar::folder_groups::build_folder_groups;

use super::agent_conversation_row::AgentConversationRow;
use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::components::notifications::notify_target::{ring_attribute, use_notify_target};
use crate::components::ui::empty::EmptyState;
use crate::components::ui::list::{List, ListRow};
use crate::components::ui::status::StatusDot;
use crate::components::ui::{IconType, SidenavGroupLabel};
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::route_session::active_session_for_path;
use crate::router_state::{use_location, use_navigate};

/// The panel.
#[component]
pub fn SidebarAgents(query: String) -> Element {
    let pump = use_store();
    let path = use_location();
    let navigate = use_navigate();
    let notify_target = use_notify_target();
    let (groups, active_session_id, agent_conversation_ids, active_agent_id) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let documents = store_navigation_documents(store, &BrowserWorkerPaths, now_ms);
        let folders = build_folder_groups(store, &BrowserWorkerPaths, now_ms);
        let groups =
            project_sidebar_agent_groups(store, &BrowserWorkerPaths, &documents, &query, &folders);
        let active = active_session_for_path(store, &BrowserWorkerPaths, &path.read())
            .map(|session| session.id.to_string());
        let needle = normalize_navigation_search_query(&query);
        let mut conversations: Vec<_> = store
            .agent_chat
            .conversations
            .values()
            .filter(|conversation| conversation.parent_id.is_none())
            .filter(|conversation| {
                needle.is_empty()
                    || [
                        conversation.title.as_str(),
                        conversation.worker_label.as_str(),
                        conversation.cwd.as_str(),
                        conversation
                            .model
                            .as_ref()
                            .map_or("", |model| model.model_id.as_str()),
                    ]
                    .iter()
                    .any(|value| normalize_navigation_search_query(value).contains(needle.as_str()))
            })
            .collect();
        conversations.sort_by(|left, right| {
            let running_order =
                |conversation: &roost_protocol::wire::agent_chat::ConversationSummary| {
                    u8::from(
                        conversation.run_state
                            != roost_protocol::wire::agent_chat::AgentRunState::Running,
                    )
                };
            running_order(left)
                .cmp(&running_order(right))
                .then_with(|| right.updated_ms.cmp(&left.updated_ms))
        });
        let agent_conversation_ids: Vec<String> = conversations
            .into_iter()
            .map(|conversation| conversation.id.clone())
            .collect();
        let active_agent = match crate::routes::Route::parse(&path.read()) {
            crate::routes::Route::Agent { conversation_id } => Some(conversation_id),
            _ => None,
        };
        (groups, active, agent_conversation_ids, active_agent)
    };
    let filter_active = !normalize_navigation_search_query(&query).is_empty();
    rsx! {
        section {
            class: "flex min-h-0 flex-1 flex-col overflow-y-auto py-1",
            "data-testid": "sidebar-agents",
            "aria-label": "Agents",
            if groups.is_empty() && agent_conversation_ids.is_empty() {
                if filter_active {
                    EmptyState { icon: IconType::SearchX, title: "No matching agents", supporting: "Try a different filter." }
                } else {
                    EmptyState {
                        icon: IconType::Bot,
                        title: "No active agents",
                        supporting: "Active coding agents appear here while their terminal sessions remain open.",
                    }
                }
            }
            if !agent_conversation_ids.is_empty() {
                section {
                    class: "flex flex-col gap-1",
                    "data-testid": "sidebar-agent-conversations",
                    "aria-label": "Roost agent",
                    SidenavGroupLabel { class: "h-auto min-w-0 gap-2 px-4 pt-2 pb-1",
                        span { class: "min-w-0 truncate", "Roost agent" }
                    }
                    List { class: "px-2",
                        for conversation_id in agent_conversation_ids {
                            AgentConversationRow {
                                key: "{conversation_id}",
                                conversation_id: conversation_id.clone(),
                                selected: active_agent_id.as_deref() == Some(conversation_id.as_str()),
                            }
                        }
                    }
                }
            }
            for group in groups {
                section {
                    key: "{group.folder.key}",
                    class: "flex flex-col gap-1",
                    "data-testid": "sidebar-agent-group-{group.folder.key}",
                    "data-folder-key": group.folder.key.clone(),
                    "aria-label": "{group.folder.name} on {group.folder.server}",
                    SidenavGroupLabel { class: "h-auto min-w-0 gap-2 px-4 pt-2 pb-1",
                        span { class: "min-w-0 truncate", {group.folder.name.clone()} }
                        span { class: "min-w-0 truncate font-normal text-muted-foreground", {group.folder.server.clone()} }
                    }
                    List { class: "px-2",
                        for row in group.rows {
                            div {
                                key: "{row.document.session_id}",
                                class: "block rounded-md",
                                "data-notify-target": notify_target.as_ref().and_then(|target| {
                                    ring_attribute(target, &row.document.session_id)
                                }),
                                "data-notify-ring": "child",
                                ListRow {
                                    leading_icon: Some(IconType::Terminal),
                                    headline: rsx! {
                                        span {
                                            "data-testid": "sidebar-agent-title-{row.document.session_id}",
                                            title: row.document.display_title.clone(),
                                            {row.document.display_title.clone()}
                                        }
                                    },
                                    support: Some(rsx! {
                                        span { class: "flex min-w-0 flex-wrap items-center gap-2",
                                            span { "data-testid": "sidebar-agent-id-{row.document.session_id}",
                                                {row.status.common.agent_id.as_str().to_owned()}
                                            }
                                            span { {agent_status_presentation(row.level).label} }
                                            if !row.document.available {
                                                span { "data-testid": "sidebar-agent-availability-{row.document.session_id}", "Unavailable" }
                                            }
                                        }
                                    }),
                                    trailing: Some(rsx! {
                                        span { class: "flex shrink-0 items-center gap-2",
                                            AgentStatusIndicator { session_id: row.document.session_id.clone(), compact: true }
                                            StatusDot {
                                                status: if row.document.available { "ok".to_owned() } else { "offline".to_owned() },
                                                title: Some(if row.document.available { "Available".to_owned() } else { "Machine unavailable".to_owned() }),
                                            }
                                        }
                                    }),
                                    href: Some(row.document.href.clone()),
                                    on_navigate: Some(agent_row_navigate(
                                        pump.clone(),
                                        navigate,
                                        row.document.session_id.clone(),
                                    )),
                                    selected: active_session_id.as_deref() == Some(row.document.session_id.as_str()),
                                    test_id: Some(format!("sidebar-agent-row-{}", row.document.session_id)),
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The in-app click on a terminal row: navigate, then record the session as the
/// last visited and close the drawer. `ListRow` only calls it for a plain primary
/// click, which is the case the old click-capturing wrapper also gated on.
fn agent_row_navigate(
    pump: crate::pump::Pump,
    navigate: EventHandler<String>,
    session_id: String,
) -> EventHandler<String> {
    EventHandler::new(move |href: String| {
        navigate.call(href);
        pump.dispatch(ClientEvent::Sidebar(SidebarIntent::RecordNavigation {
            session_id: session_id.clone(),
            last_workspace: None,
            close_drawer: true,
        }));
    })
}
