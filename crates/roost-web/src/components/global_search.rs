//! `/search`: the fleet-wide search page — one field over session metadata and
//! the retained terminal content of every machine, mounted as the main pane's
//! `Search` overlay above the deck.
//!
//! The overlay lives here so crossing `/s` → `/search` → `/s` keeps the deck and
//! every pane mounted, which is the whole contract of the main pane's overlay
//! arm (`components::main_pane`). What the page OWNS is thin on purpose: the
//! rows are the client's navigation projection and the coordinator's search
//! ledger, the query is the route, and the state machine that fences a page
//! against the search that asked for it belongs to `roost_client_core`. What is
//! left for this module is reading that state, asking for the pages it has not
//! read yet, and telling the reader what it has.
//!
//! Ports `apps/web/src/components/search/GlobalSearchPage.tsx` and
//! `GlobalSearchContentResults.tsx`, over `roost_client_core::client::global_search`.

pub mod content;
pub mod driver;
pub mod metadata;
pub mod query;

use dioxus::prelude::*;

use crate::components::global_search::query::{SearchRouteQuery, SearchScope};
use crate::components::terminal::dom::{now_ms, sleep_ms};
use crate::pump::use_store;
use crate::router_state::{use_location, use_navigate};

/// A frame past the controller's own settle: the host arrives a moment after
/// the deadline rather than on it, so a slow frame costs a keystroke's delay
/// and not a scan that never starts.
const FIRST_PAGE_DELAY_MS: u64 =
    roost_client_core::client::global_search::GLOBAL_SEARCH_DEBOUNCE_MS + 16;

/// The search page, for the main pane's `Search` overlay.
#[component]
pub fn GlobalSearchOverlay() -> Element {
    let pump = use_store();
    let path = use_location();
    let navigate = use_navigate();
    let route_query = SearchRouteQuery::parse(&path());

    // The route IS the query, so a new address is a new search and an abandoned
    // one is cancelled. This runs on the route, not on the store: the fold that
    // publishes a page must not re-arm the query it just answered.
    let arming = pump.clone();
    let arming_effect = arming.clone();
    use_effect(use_reactive((&route_query,), move |(query,)| {
        let _ = driver::set_query(&arming_effect, query.content_query(), now_ms());
        if query.scope == SearchScope::Attention {
            return;
        }
        let Some(search_id) = driver::mint_search_id() else {
            tracing::warn!(target: "search", "no search identity available; content search off");
            return;
        };
        let page_pump = arming_effect.clone();
        spawn(async move {
            sleep_ms(FIRST_PAGE_DELAY_MS).await;
            driver::take_due_first_page(&page_pump, search_id);
        });
    }));

    // Leaving the page abandons the scan: the coordinator's ledger would keep
    // it running for the rest of the cursor's lifetime, and nobody is reading.
    use_drop({
        let pump = pump.clone();
        move || {
            let abandoned = {
                let core = pump.core();
                let mut core = core.borrow_mut();
                core.store_mut().global_search.stop_logical_search()
            };
            if let Some(search_id) = abandoned {
                driver::cancel(&pump, search_id);
            }
        }
    });

    let update = EventHandler::new(move |next: SearchRouteQuery| navigate.call(next.to_path()));

    rsx! {
        section {
            class: "df-search-page",
            "data-testid": "global-search-page",
            aria_labelledby: Some("global-search-title".to_owned()),
            style: "flex: 1; display: flex; flex-direction: column; overflow: hidden;",
            div { class: "df-search-header",
                h1 { id: "global-search-title", class: "md-headline-s", style: "margin: 0;",
                    "Search sessions"
                }
                p { class: "md-body-m", style: "margin: 0;",
                    "Find sessions by metadata and search retained terminal content across every machine."
                }
                crate::components::md::TextField {
                    value: route_query.text.clone(),
                    label: Some(route_query.search_label().to_owned()),
                    placeholder: Some(match route_query.scope {
                        SearchScope::All => "Title, path, or terminal text…",
                        SearchScope::Attention => "Title, path, agent status…",
                    }.to_owned()),
                    test_id: Some("global-search-input".to_owned()),
                    autofocus: true,
                    on_input: {
                        let query = route_query.clone();
                        move |text: String| update.call(query.with_text(&text))
                    },
                }
                div { role: "group", "aria-label": "Search scope",
                    style: "display: flex; gap: var(--md-space-2); flex-wrap: wrap;",
                    crate::components::md::Chip {
                        label: "All sessions".to_owned(),
                        icon: Some((if route_query.scope == SearchScope::All { "check" } else { "terminal" }).to_owned()),
                        selected: Some(route_query.scope == SearchScope::All),
                        test_id: Some("global-search-scope-all".to_owned()),
                        onclick: {
                            let query = route_query.clone();
                            move |_| update.call(SearchRouteQuery { scope: SearchScope::All, ..query.clone() })
                        },
                    }
                    crate::components::md::Chip {
                        label: "Needs attention".to_owned(),
                        icon: Some((if route_query.scope == SearchScope::Attention { "check" } else { "notifications" }).to_owned()),
                        selected: Some(route_query.scope == SearchScope::Attention),
                        test_id: Some("global-search-scope-attention".to_owned()),
                        onclick: {
                            let query = route_query.clone();
                            move |_| update.call(SearchRouteQuery { scope: SearchScope::Attention, ..query.clone() })
                        },
                    }
                    if route_query.scope == SearchScope::All {
                        crate::components::md::Chip {
                            label: "Match terminal case".to_owned(),
                            icon: Some((if route_query.case_sensitive { "check" } else { "match_case" }).to_owned()),
                            selected: Some(route_query.case_sensitive),
                            test_id: Some("global-search-case-sensitive".to_owned()),
                            onclick: {
                                let query = route_query.clone();
                                move |_| update.call(SearchRouteQuery { case_sensitive: !query.case_sensitive, ..query.clone() })
                            },
                        }
                    }
                }
            }
            div { class: "df-search-scroll",
                style: "flex: 1; overflow: auto; display: flex; flex-direction: column; gap: var(--md-space-4);",
                if route_query.scope == SearchScope::All {
                    content::ContentResults { route_query: route_query.clone() }
                }
                metadata::MetadataResults { route_query: route_query.clone() }
            }
        }
    }
}
