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
use roost_client_core::client::agents::status_policy::{
    AgentStatusLevel, agent_status_presentation,
};
use roost_client_core::store::navigation::query::{
    attention_navigation_documents, filter_navigation_search_documents,
};
use roost_client_core::store::navigation::{NavigationSearchAttention, NavigationSearchDocument};
use roost_client_core::store::sidebar::documents::store_navigation_documents;

use crate::components::global_search::query::{SearchRouteQuery, SearchScope};
use crate::components::settings::format::relative_time;
use crate::components::ui::{
    Card, CardContent, CardHeader, CardTitle, CommandGroup, CommandGroupLabel, CommandItem,
    CommandList, EmptyState, Icon, IconType, StatusDot,
};
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
        section {
            class: "flex flex-col gap-2",
            aria_labelledby: "global-search-metadata-title",
            Card {
                CardHeader {
                    CardTitle {
                        span { id: "global-search-metadata-title",
                            match route_query.scope {
                                SearchScope::Attention => "Agent attention",
                                SearchScope::All => "Session metadata",
                            }
                        }
                    }
                    div { class: "text-sm font-medium", role: "status", "aria-live": "polite", "aria-atomic": "true",
                        {summary}
                    }
                }
                CardContent {
                    if rows.is_empty() {
                        EmptyState {
                            icon: match route_query.scope {
                                SearchScope::Attention => IconType::BellOff,
                                SearchScope::All => IconType::SearchX,
                            },
                            title: empty_title(route_query.scope, &route_query.text),
                            supporting: Some(empty_supporting(route_query.scope, &route_query.text).to_owned()),
                        }
                    } else {
                        div { "data-testid": "global-search-results",
                            class: "flex flex-col gap-3",
                            CommandList { class: "flex flex-col gap-3 max-h-none min-h-0 overflow-visible",
                                for group in group_documents(&rows) {
                                    CommandGroup {
                                        CommandGroupLabel { "{group.folder} · {group.machine}" }
                                        for document in group.documents {
                                            MetadataRow {
                                                document: (*document).clone(),
                                                now_ms,
                                                query: route_query.text.clone(),
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
            }
        }
    }
}

/// One session row beneath its folder and machine heading.
#[component]
fn MetadataRow(
    document: NavigationSearchDocument,
    now_ms: i64,
    query: String,
    on_open: EventHandler<()>,
) -> Element {
    let session_id = document.session_id.clone();
    let activity_ms = u64::try_from(document.activity_at).unwrap_or(0);
    let now = u64::try_from(now_ms).unwrap_or(0);
    let activity = if document.available {
        relative_time(now, activity_ms)
    } else {
        "Unavailable".to_owned()
    };
    rsx! {
        CommandItem {
            value: session_id.clone(),
            class: "flex items-center gap-3",
            test_id: format!("global-search-result-{session_id}"),
            show_check: false,
            on_select: move |()| on_open.call(()),
            Icon {
                icon: if document.agent_id.is_some() { IconType::Bot } else { IconType::Terminal },
                class: "size-4 shrink-0 text-muted-foreground",
            }
            span { class: "min-w-0 flex-1 truncate text-sm font-medium text-foreground",
                "data-testid": format!("global-search-title-{session_id}"),
                HighlightedText { text: document.display_title.clone(), query }
            }
            span { class: "flex shrink-0 items-center gap-2 whitespace-nowrap text-xs text-muted-foreground",
                if let Some(label) = attention_label(document.agent_attention) {
                    span { "data-testid": format!("global-search-attention-{session_id}"), {label} }
                }
                StatusDot {
                    status: (if document.available { "ok" } else { "offline" }).to_owned(),
                    title: (if document.available { "Available" } else { "Machine unavailable" }).to_owned(),
                }
                span { "data-testid": format!("global-search-availability-{session_id}"), {activity} }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupedDocuments<'a> {
    folder: String,
    machine: String,
    folder_key: String,
    documents: Vec<&'a NavigationSearchDocument>,
}

fn group_documents(documents: &[NavigationSearchDocument]) -> Vec<GroupedDocuments<'_>> {
    let mut groups: Vec<GroupedDocuments<'_>> = Vec::new();
    for document in documents {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.folder_key == document.folder_key)
        {
            group.documents.push(document);
        } else {
            groups.push(GroupedDocuments {
                folder: folder_label(document),
                machine: document.worker_label.clone(),
                folder_key: document.folder_key.clone(),
                documents: vec![document],
            });
        }
    }
    groups
}

fn folder_label(document: &NavigationSearchDocument) -> String {
    document.workspace_name.clone().unwrap_or_else(|| {
        document
            .cwd
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or(&document.cwd)
            .to_owned()
    })
}

#[component]
pub fn HighlightedText(text: String, query: String) -> Element {
    let spans = highlighted_spans(&text, &query);
    rsx! {
        for (part, matched) in spans {
            if matched {
                span { class: "rounded-sm bg-accent text-accent-foreground", {part} }
            } else {
                span { {part} }
            }
        }
    }
}

fn highlighted_spans(text: &str, query: &str) -> Vec<(String, bool)> {
    let lower_text = text.to_lowercase();
    let mut ranges = query
        .split_whitespace()
        .filter_map(|term| {
            let needle = term.to_lowercase();
            let start = lower_text.find(&needle)?;
            Some((start, start + needle.len()))
        })
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    let mut spans = Vec::new();
    let mut cursor = 0;
    for (start, end) in ranges {
        if start < cursor {
            continue;
        }
        let (Some(before), Some(matched)) = (text.get(cursor..start), text.get(start..end)) else {
            continue;
        };
        if !before.is_empty() {
            spans.push((before.to_owned(), false));
        }
        spans.push((matched.to_owned(), true));
        cursor = end;
    }
    if cursor < text.len() {
        spans.push((text[cursor..].to_owned(), false));
    }
    if spans.is_empty() {
        vec![(text.to_owned(), false)]
    } else {
        spans
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

/// The word a row shows for what its agent wants: the shared presentation
/// table's label, so the row says what the status badge says ("Needs input").
fn attention_label(attention: Option<NavigationSearchAttention>) -> Option<&'static str> {
    let level = match attention? {
        NavigationSearchAttention::Blocked => AgentStatusLevel::Blocked,
        NavigationSearchAttention::Done => AgentStatusLevel::Done,
    };
    Some(agent_status_presentation(level).label)
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

    fn document(folder_key: &str, title: &str, cwd: &str) -> NavigationSearchDocument {
        NavigationSearchDocument {
            session_id: title.to_owned(),
            href: String::new(),
            display_title: title.to_owned(),
            custom_title: None,
            terminal_title: None,
            cwd: cwd.to_owned(),
            spawn_cwd: None,
            workspace_id: None,
            workspace_name: None,
            folder_key: folder_key.to_owned(),
            worker_label: "m1".to_owned(),
            worker_fp: String::new(),
            git_branch: None,
            git_remote: None,
            pull_request_number: None,
            pull_request_state: None,
            pull_request_checks: None,
            pull_request_url: None,
            port_label: None,
            search_text: String::new(),
            activity_at: 0,
            available: true,
            agent_status: None,
            agent_attention: None,
            agent_unseen: false,
            agent_id: None,
            agent_message: None,
            agent_updated_at_ms: None,
            agent_arrival: 0,
            client_only: false,
        }
    }

    #[test]
    fn search_results_group_by_folder_without_reordering_siblings_or_groups() {
        let documents = [
            document("folder-a", "alpha", "/work/alpha"),
            document("folder-b", "beta", "/work/beta"),
            document("folder-a", "alpha-two", "/work/alpha"),
        ];

        let groups = group_documents(&documents);

        assert_eq!(
            groups
                .iter()
                .map(|group| group.folder.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        assert_eq!(
            groups[0]
                .documents
                .iter()
                .map(|document| document.display_title.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "alpha-two"]
        );
        assert_eq!(groups[0].machine, "m1");
    }

    #[test]
    fn matching_title_highlight_is_case_insensitive_and_preserves_unmatched_text() {
        assert_eq!(
            highlighted_spans("Terminal Alpha", "alpha TERMINAL"),
            [
                ("Terminal".to_owned(), true),
                (" ".to_owned(), false),
                ("Alpha".to_owned(), true),
            ]
        );
        assert_eq!(
            folder_label(&document("f", "title", "/work/folder")),
            "folder"
        );
    }
}
