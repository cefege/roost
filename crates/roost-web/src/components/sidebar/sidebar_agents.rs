//! The Agents panel: open sessions with a known coding-agent status, grouped
//! under the Folders panel's folder order and ordered by attention, each a
//! list row linking to its terminal. Ports
//! `apps/web/src/components/sidebar/SidebarAgents.tsx`; `SidebarRoot` renders
//! it. The grouping is `store::sidebar::agents_projection`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::client::agents::status_policy::agent_status_presentation;
use roost_client_core::store::navigation::query::normalize_navigation_search_query;
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::sidebar::agents_projection::project_sidebar_agent_groups;
use roost_client_core::store::sidebar::documents::store_navigation_documents;
use roost_client_core::store::sidebar::folder_groups::build_folder_groups;

use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::components::md::{EmptyState, List, ListRow, StatusDot};
use crate::components::notifications::notify_target::{ring_attribute, use_notify_target};
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
    let (groups, active_session_id) = {
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
        (groups, active)
    };
    let filter_active = !normalize_navigation_search_query(&query).is_empty();
    rsx! {
        section { class: "workbench-sidebar-agents", "data-testid": "sidebar-agents", "aria-label": "Agents",
            if groups.is_empty() {
                if filter_active {
                    EmptyState { icon: "search_off", title: "No matching agents", supporting: "Try a different filter." }
                } else {
                    EmptyState {
                        icon: "smart_toy",
                        title: "No active agents",
                        supporting: "Active coding agents appear here while their terminal sessions remain open.",
                    }
                }
            }
            for group in groups {
                section {
                    key: "{group.folder.key}",
                    class: "workbench-sidebar-agents__group",
                    "data-testid": "sidebar-agent-group-{group.folder.key}",
                    "data-folder-key": group.folder.key.clone(),
                    "aria-label": "{group.folder.name} on {group.folder.server}",
                    h3 { class: "workbench-sidebar-agents__group-title",
                        span { class: "workbench-sidebar-agents__group-name", {group.folder.name.clone()} }
                        span { class: "workbench-sidebar-agents__group-server", {group.folder.server.clone()} }
                    }
                    List { class: "workbench-sidebar-agents__list",
                        for row in group.rows {
                            div {
                                key: "{row.document.session_id}",
                                class: "workbench-sidebar-agents__row",
                                "data-notify-target": notify_target.as_ref().and_then(|target| {
                                    ring_attribute(target, &row.document.session_id)
                                }),
                                onclick: {
                                    let pump = pump.clone();
                                    let session_id = row.document.session_id.clone();
                                    move |event: MouseEvent| {
                                        let primary = event.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary);
                                        if !primary || !event.modifiers().is_empty() {
                                            return;
                                        }
                                        pump.dispatch(ClientEvent::Sidebar(SidebarIntent::RecordNavigation {
                                            session_id: session_id.clone(),
                                            last_workspace: None,
                                            close_drawer: true,
                                        }));
                                    }
                                },
                                ListRow {
                                    leading_icon: "terminal",
                                    headline: rsx! {
                                        span {
                                            "data-testid": "sidebar-agent-title-{row.document.session_id}",
                                            title: row.document.display_title.clone(),
                                            {row.document.display_title.clone()}
                                        }
                                    },
                                    support: rsx! {
                                        span { class: "workbench-sidebar-agents__metadata",
                                            span { "data-testid": "sidebar-agent-id-{row.document.session_id}",
                                                {row.status.common.agent_id.as_str().to_owned()}
                                            }
                                            span { {agent_status_presentation(row.level).label} }
                                            if !row.document.available {
                                                span { "data-testid": "sidebar-agent-availability-{row.document.session_id}", "Unavailable" }
                                            }
                                        }
                                    },
                                    trailing: rsx! {
                                        span { class: "workbench-sidebar-agents__trailing",
                                            AgentStatusIndicator { session_id: row.document.session_id.clone(), compact: true }
                                            StatusDot {
                                                status: if row.document.available { "ok" } else { "offline" },
                                                title: if row.document.available { "Available" } else { "Machine unavailable" },
                                            }
                                        }
                                    },
                                    href: row.document.href.clone(),
                                    on_navigate: navigate,
                                    selected: active_session_id.as_deref() == Some(row.document.session_id.as_str()),
                                    test_id: "sidebar-agent-row-{row.document.session_id}",
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
