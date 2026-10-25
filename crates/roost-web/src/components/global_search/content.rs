//! The terminal-content half of `/search`: the coordinator's retained-row
//! matches, joined to the sessions they belong to, paged by cursor and opened by
//! handing the literal to that session's own pane.
//!
//! The rows are the COORDINATOR's, not this client's findings — the query runs
//! against its search ledger, the only party that knows every machine's
//! retained history — so a match is joined to a session through the same
//! navigation projection the metadata list reads. A match whose session is not
//! in that projection is named as such rather than dropped: silently hiding it
//! would report the fleet as having fewer matches than it has.
//!
//! Ports `apps/web/src/components/search/GlobalSearchContentResults.tsx`.

use dioxus::prelude::*;
use roost_client_core::client::global_search::GlobalSearchResults;
use roost_client_core::search::global::{GlobalSearchMatch, GlobalSearchPartial};
use roost_client_core::store::navigation::NavigationSearchDocument;
use roost_client_core::store::sidebar::documents::store_navigation_documents;
use roost_web_terminal::find::hits::PreferredMatch;
use roost_web_terminal::find::intent::TerminalFindIntentOptions;

use crate::components::global_search::{driver, query::SearchRouteQuery};
use crate::components::ui::{
    Button, ButtonGroup, ButtonVariant, Card, CardContent, CardHeader, CardSize, CardTitle,
    CommandItem, CommandList, EmptyState, Icon, IconType,
};
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;

/// One match, with the session row it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinedMatch {
    /// The coordinator's row.
    pub candidate: GlobalSearchMatch,
    /// The session, when this client can still see it.
    pub document: Option<NavigationSearchDocument>,
}

/// The content panel for the current route query.
#[component]
pub fn ContentResults(route_query: SearchRouteQuery) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let (results, documents, waiting) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let documents = store_navigation_documents(store, &BrowserWorkerPaths, now_ms);
        let search = store.global_search.results().clone();
        let waiting = store.global_search.is_debouncing() || store.global_search.is_loading();
        (search, documents, waiting)
    };
    let rows = join_matches(&results.matches, &documents);
    let missing = results
        .matches
        .len()
        .saturating_sub(rows.iter().filter(|row| row.document.is_some()).count());
    let incomplete = incomplete(&results, missing);
    let searching = !route_query.text.trim().is_empty();
    let unsearched = results
        .eligible_sessions
        .saturating_sub(results.searched_sessions);
    let eligible = results.eligible_sessions;
    let unsearched_noun = if unsearched == 1 {
        "session was"
    } else {
        "sessions were"
    };
    let missing_noun = if missing == 1 {
        "match belongs"
    } else {
        "matches belong"
    };

    rsx! {
        Card {
            size: CardSize::Sm,
            role: "region",
            "aria-labelledby": "global-content-search-title",
            CardHeader {
                CardTitle {
                    span { id: "global-content-search-title", "Terminal content" }
                }
                div {
                    class: "text-sm",
                    role: "status",
                    "aria-live": "polite",
                    "aria-atomic": "true",
                    "data-testid": "global-content-summary",
                    {summary(&results, &rows, waiting, &route_query.text)}
                }
            }
            CardContent { class: "flex min-w-0 flex-col gap-3",
                if searching {
                    if rows.is_empty() {
                        if !waiting {
                            EmptyState {
                                icon: if results.error.is_some() { IconType::CircleAlert } else { IconType::SearchX },
                                title: if results.error.is_some() { "Terminal content search failed".to_owned() } else if incomplete { "No matches in the completed portion".to_owned() } else { "No terminal content matches".to_owned() },
                                supporting: Some(
                                    results
                                        .error
                                        .clone()
                                        .unwrap_or_else(|| {
                                            if incomplete {
                                                "Some sessions or retained rows could not be searched.".to_owned()
                                            } else {
                                                "No retained terminal row contains this literal query.".to_owned()
                                            }
                                        }),
                                ),
                                action: results.retryable.then(|| rsx! {
                                    Button {
                                        variant: ButtonVariant::Secondary,
                                        icon: Some(IconType::RefreshCw),
                                        "data-testid": "global-content-retry",
                                        onclick: {
                                            let pump = pump.clone();
                                            move |_| driver::retry(&pump)
                                        },
                                        "Retry"
                                    }
                                }),
                            }
                        }
                    } else {
                        CommandList { class: "flex flex-col gap-2 max-h-none min-h-0 overflow-visible",
                            for (index, row) in rows.iter().enumerate() {
                                ContentRow {
                                    row: row.clone(),
                                    index,
                                    on_open: {
                                        let open = open_result(&pump, navigate, row, &route_query);
                                        move |_| open.call(())
                                    },
                                }
                            }
                        }
                    }
                    if incomplete {
                        Card {
                            size: CardSize::Sm,
                            "data-testid": "global-content-incomplete",
                            role: if results.error.is_some() { "alert" } else { "status" },
                            "aria-live": if results.error.is_some() { "assertive" } else { "polite" },
                            CardContent { class: "flex flex-col gap-2",
                                span { class: "text-sm font-medium", "Search incomplete" }
                                if results.next_cursor.is_some() {
                                    span { class: "text-sm",
                                        "More retained terminal rows are available."
                                    }
                                }
                                if results.eligible_sessions > 0
                                    && results.searched_sessions < results.eligible_sessions
                                {
                                    span {
                                        class: "text-sm",
                                        "data-testid": "global-content-unsearched",
                                        "{unsearched} of {eligible} eligible {unsearched_noun} not searched."
                                    }
                                }
                                if results.truncated && results.next_cursor.is_none() {
                                    span { class: "text-sm",
                                        "The bounded page ended before every retained row could be searched."
                                    }
                                }
                                if missing > 0 {
                                    span { class: "text-sm",
                                        "{missing} {missing_noun} sessions no longer present in the current session list."
                                    }
                                }
                                for partial in results.partials.iter() {
                                    span {
                                        class: "text-sm",
                                        "data-testid": format!("global-content-partial-{}", partial.session_id.as_str()),
                                        {partial_line(partial, &documents)}
                                    }
                                }
                            }
                        }
                    }
                    if results.next_cursor.is_some() && results.error.is_none() {
                        div {
                            role: "group",
                            "aria-label": "Terminal content pagination",
                            class: "flex justify-center",
                            ButtonGroup {
                                Button {
                                    variant: ButtonVariant::Secondary,
                                    icon: Some(IconType::ChevronDown),
                                    disabled: waiting,
                                    "data-testid": "global-content-load-more",
                                    onclick: {
                                        let pump = pump.clone();
                                        move |_| {
                                            driver::take_next_page(&pump);
                                        }
                                    },
                                    if waiting {
                                        "Loading…"
                                    } else {
                                        "Load more"
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

/// One match: the session it is in, the retained row that matched, and where
/// that row lives.
#[component]
fn ContentRow(row: JoinedMatch, index: usize, on_open: EventHandler<()>) -> Element {
    let Some(document) = row.document.clone() else {
        return rsx! {};
    };
    let session_id = document.session_id.clone();
    rsx! {
        CommandItem {
            value: format!("{session_id}-{index}"),
            class: "flex items-start gap-3",
            test_id: format!("global-content-result-{session_id}-{index}"),
            show_check: false,
            on_select: move |()| on_open.call(()),
            Icon {
                icon: IconType::FileSearch,
                class: "mt-0.5 size-4 shrink-0 text-muted-foreground",
            }
            span { class: "flex min-w-0 flex-1 flex-col gap-1",
                span {
                    class: "text-sm font-medium text-foreground",
                    "data-testid": format!("global-content-title-{session_id}"),
                    {document.display_title.clone()}
                }
                span { class: "flex flex-col gap-1 text-xs text-muted-foreground",
                    span {
                        "data-testid": format!("global-content-preview-{session_id}-{index}"),
                        class: "whitespace-pre-wrap [overflow-wrap:anywhere]",
                        {row.candidate.preview.clone()}
                    }
                    span { "{document.cwd} · {document.worker_label}" }
                }
            }
            span { class: "shrink-0 whitespace-nowrap text-xs text-muted-foreground",
                "Open find"
            }
        }
    }
}

/// Hand the literal to that session's pane, then open it.
///
/// The find request goes FIRST: the pane is often cold, so the intent is held
/// until it mounts, and navigating is what mounts it. Reversing the two would
/// race the deck against the answer the reader asked for.
fn open_result(
    pump: &Pump,
    navigate: EventHandler<String>,
    row: &JoinedMatch,
    route_query: &SearchRouteQuery,
) -> EventHandler<()> {
    let Some(document) = row.document.clone() else {
        return EventHandler::new(|_| {});
    };
    let literal = route_query.text.trim().to_owned();
    let case_sensitive = route_query.case_sensitive;
    let coordinate = u32::try_from(row.candidate.row)
        .ok()
        .map(|number| PreferredMatch {
            grid_epoch: row.candidate.grid_epoch.clone(),
            row: number,
            col: row.candidate.col,
        });
    let pump = pump.clone();
    EventHandler::new(move |_| {
        pump.request_terminal_find(
            &document.session_id,
            &literal,
            TerminalFindIntentOptions {
                case_sensitive: Some(case_sensitive),
                preferred_global_match: coordinate.clone(),
            },
        );
        navigate.call(document.href.clone());
    })
}

/// The matches, each joined to the session row the projection knows.
#[must_use]
pub fn join_matches(
    matches: &[GlobalSearchMatch],
    documents: &[NavigationSearchDocument],
) -> Vec<JoinedMatch> {
    matches
        .iter()
        .map(|candidate| JoinedMatch {
            candidate: candidate.clone(),
            document: documents
                .iter()
                .find(|document| document.session_id == candidate.session_id.as_str())
                .cloned(),
        })
        .collect()
}

/// Whether the answer is anything short of the whole fleet.
#[must_use]
pub fn incomplete(results: &GlobalSearchResults, missing_projections: usize) -> bool {
    results.truncated
        || results.next_cursor.is_some()
        || !results.partials.is_empty()
        || missing_projections > 0
        || (results.eligible_sessions > 0 && results.searched_sessions < results.eligible_sessions)
        || (results.error.is_some() && !results.matches.is_empty())
}

/// The sentence under the panel's heading.
#[must_use]
pub fn summary(
    results: &GlobalSearchResults,
    rows: &[JoinedMatch],
    waiting: bool,
    query: &str,
) -> String {
    if query.trim().is_empty() {
        return "Search retained terminal content with the field above.".to_owned();
    }
    if waiting && !results.has_searched {
        return "Searching retained terminal content…".to_owned();
    }
    if let Some(error) = results.error.as_ref() {
        return format!("Terminal content search failed: {error}");
    }
    let matches = rows.iter().filter(|row| row.document.is_some()).count();
    let noun = if matches == 1 { "match" } else { "matches" };
    if results.eligible_sessions == 0 {
        return format!("{matches} {noun}");
    }
    if results.searched_sessions < results.eligible_sessions {
        format!(
            "{matches} {noun} across {} of {} sessions — coverage is partial",
            results.searched_sessions, results.eligible_sessions
        )
    } else {
        format!(
            "{matches} {noun} across all {} sessions searched",
            results.eligible_sessions
        )
    }
}

/// One partial, named for the session it belongs to.
#[must_use]
pub fn partial_line(
    partial: &GlobalSearchPartial,
    documents: &[NavigationSearchDocument],
) -> String {
    let name = documents
        .iter()
        .find(|document| document.session_id == partial.session_id.as_str())
        .map(|document| document.display_title.clone())
        .unwrap_or_else(|| "A session no longer in the current view".to_owned());
    format!("{name} {}.", partial.reason.label())
}
