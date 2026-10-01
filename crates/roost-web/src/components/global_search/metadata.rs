//! The metadata half of `/search`: every session the client can see, filtered by
//! the same literal, and where the reader goes when they pick one.
//!
//! Rows come from the client's navigation projection — the same
//! `store_navigation_documents` the command palette reads — so a session that
//! is searchable here is searchable there, and a title rename that repaints the
//! sidebar repaints this list in the same revision. The content panel above
//! joins through the same projection, which is why a content result can name
//! the session it belongs to.
//!
//! Ports `apps/web/src/components/search/GlobalSearchPage.tsx:195-307`.

use dioxus::prelude::*;
use roost_client_core::store::navigation::query::{
    attention_navigation_documents, filter_navigation_search_documents,
};
use roost_client_core::store::navigation::{NavigationSearchAttention, NavigationSearchDocument};
use roost_client_core::store::sidebar::documents::store_navigation_documents;

use crate::components::global_search::query::{SearchRouteQuery, SearchScope};
use crate::components::md::{EmptyState, List, ListRow, StatusDot, Surface, SurfaceRadius};
use crate::components::settings::format::relative_time;
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::router_state::use_navigate;

/// The metadata list for the current route query.
#[component]
pub fn MetadataResults(route_query: SearchRouteQuery) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let (rows, now_ms) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let documents = store_navigation_documents(store, &BrowserWorkerPaths, now_ms);
        let scoped: Vec<NavigationSearchDocument> = match route_query.scope {
            SearchScope::All => documents.clone(),
            SearchScope::Attention => attention_navigation_documents(&documents)
                .into_iter()
                .cloned()
                .collect(),
        };
        let rows = filter_navigation_search_documents(&scoped, &route_query.text)
            .into_iter()
            .cloned()
            .collect::<Vec<NavigationSearchDocument>>();
        (rows, now_ms)
    };
    let summary = metadata_summary(route_query.scope, rows.len(), &route_query.text);

    rsx! {
        Surface { class: "df-search-metadata", level: 1, radius: SurfaceRadius::Lg,
            pad: 4,
            border: true,
            aria_labelledby: Some("global-search-metadata-title".to_owned()),
            style: "display: flex; flex-direction: column; gap: var(--md-space-2);",
            h2 { id: "global-search-metadata-title", class: "md-title-m", style: "margin: 0;",
                match route_query.scope {
                    SearchScope::Attention => "Agent attention",
                    SearchScope::All => "Session metadata",
                }
            }
            div { class: "md-label-m", role: "status", "aria-live": "polite", "aria-atomic": "true",
                {summary}
            }
            if rows.is_empty() {
                EmptyState {
                    icon: (match route_query.scope {
                        SearchScope::Attention => "notifications_none",
                        SearchScope::All => "search_off",
                    }).to_owned(),
                    title: empty_title(route_query.scope, &route_query.text),
                    supporting: Some(empty_supporting(route_query.scope, &route_query.text).to_owned()),
                }
            } else {
                div { "data-testid": "global-search-results",
                    style: "display: flex; flex-direction: column; gap: var(--md-space-2);",
                    List { contained: true,
                        for document in rows.iter() {
                            MetadataRow {
                                document: document.clone(),
                                now_ms,
                                on_open: {
                                    let href = document.href.clone();
                                    move |_| navigate.call(href.clone())
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One session: its title, its folder, the machine it is on, and whether an
/// operator could get to it right now.
#[component]
fn MetadataRow(
    document: NavigationSearchDocument,
    now_ms: i64,
    on_open: EventHandler<()>,
) -> Element {
    let metadata = [
        document.workspace_name.clone(),
        Some(document.worker_label.clone()),
        document
            .git_branch
            .clone()
            .map(|branch| format!("branch {branch}")),
        document.git_remote.clone(),
        document
            .pull_request_number
            .map(|number| format!("PR #{number}")),
        document.port_label.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<String>>()
    .join(" · ");
    let session_id = document.session_id.clone();
    let activity_ms = u64::try_from(document.activity_at).unwrap_or(0);
    let now = u64::try_from(now_ms).unwrap_or(0);
    let availability = if document.available {
        relative_time(now, activity_ms)
    } else {
        "Unavailable".to_owned()
    };
    rsx! {
        ListRow {
            leading_icon: Some("terminal".to_owned()),
            headline: rsx! {
                span { "data-testid": format!("global-search-title-{session_id}"),
                    {document.display_title.clone()}
                }
            },
            support: rsx! {
                span { style: "display: flex; flex-direction: column; gap: var(--md-space-1);",
                    span { {document.cwd.clone()} }
                    if !metadata.is_empty() {
                        span { {metadata} }
                    }
                    if let Some(message) = document.agent_message.clone() {
                        span { {message} }
                    }
                }
            },
            trailing: rsx! {
                span { style: "display: flex; align-items: center; gap: var(--md-space-2); white-space: nowrap;",
                    if let Some(label) = attention_label(document.agent_attention) {
                        span { "data-testid": format!("global-search-attention-{session_id}"),
                            {label}
                        }
                    }
                    StatusDot {
                        status: (if document.available { "ok" } else { "offline" }).to_owned(),
                        title: (if document.available { "Available" } else { "Machine unavailable" }).to_owned(),
                    }
                    span { "data-testid": format!("global-search-availability-{session_id}"),
                        {availability}
                    }
                }
            },
            onclick: move |_| on_open.call(()),
            test_id: Some(format!("global-search-result-{session_id}")),
        }
    }
}

/// What the status line above the list says.
fn metadata_summary(scope: SearchScope, count: usize, query: &str) -> String {
    if count > 0 {
        return if count == 1 {
            "1 session".to_owned()
        } else {
            format!("{count} sessions")
        };
    }
    if !query.trim().is_empty() {
        return "0 sessions. No matching session metadata".to_owned();
    }
    match scope {
        SearchScope::Attention => "0 sessions. Nothing needs attention",
        SearchScope::All => "0 sessions. No sessions to search",
    }
    .to_owned()
}

fn empty_title(scope: SearchScope, query: &str) -> &'static str {
    match (scope, query.trim().is_empty()) {
        (SearchScope::Attention, true) => "Nothing needs attention",
        (SearchScope::Attention, false) => "No matching sessions",
        (SearchScope::All, true) => "No sessions to search",
        (SearchScope::All, false) => "No matching session metadata",
    }
}

fn empty_supporting(scope: SearchScope, query: &str) -> &'static str {
    match (scope, query.trim().is_empty()) {
        (SearchScope::Attention, true) => "Blocked agents and unseen completions appear here.",
        (SearchScope::Attention, false) => {
            "Try another title, path, workspace, machine, or agent term."
        }
        (SearchScope::All, true) => "Sessions appear here as they open.",
        (SearchScope::All, false) => {
            "Terminal content matches appear above. Try another metadata term to filter this list."
        }
    }
}

/// The word a row shows for what its agent wants.
fn attention_label(attention: Option<NavigationSearchAttention>) -> Option<&'static str> {
    match attention {
        Some(NavigationSearchAttention::Blocked) => Some("Blocked"),
        Some(NavigationSearchAttention::Done) => Some("Done"),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_counts_rows_before_it_explains_an_empty_list() {
        assert_eq!(
            metadata_summary(SearchScope::All, 0, ""),
            "0 sessions. No sessions to search"
        );
        assert_eq!(metadata_summary(SearchScope::All, 1, "tmp"), "1 session");
        assert_eq!(metadata_summary(SearchScope::All, 3, "tmp"), "3 sessions");
        assert_eq!(
            metadata_summary(SearchScope::Attention, 0, "blocked"),
            "0 sessions. No matching session metadata"
        );
    }
}
