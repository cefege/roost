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
use crate::components::ui::{Command, CommandInput, CommandItem, Icon, IconType};
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
            class: "flex flex-1 flex-col overflow-hidden",
            "data-testid": "global-search-page",
            "aria-labelledby": "global-search-title",
            div { class: "flex flex-col gap-3 p-4",
                h1 { id: "global-search-title", class: "m-0 text-2xl font-semibold",
                    "Search sessions"
                }
                p { class: "m-0 text-sm text-muted-foreground",
                    "Find sessions by metadata and search retained terminal content across every machine."
                }
                Command {
                    should_filter: false,
                    class: "gap-3 overflow-visible text-foreground",
                    CommandInput {
                        value: route_query.text.clone(),
                        aria_label: route_query.search_label().to_owned(),
                        placeholder: match route_query.scope {
                            SearchScope::All => "Title, path, or terminal text…",
                            SearchScope::Attention => "Title, path, agent status…",
                        },
                        test_id: "global-search-input",
                        autofocus: true,
                        class: "rounded-md border border-border bg-background px-3",
                        on_search_change: {
                            let query = route_query.clone();
                            move |text: String| update.call(query.with_text(&text))
                        },
                    }
                    div { role: "group", "aria-label": "Search scope", class: "flex flex-wrap gap-2",
                        CommandItem {
                            value: "all-sessions",
                            selected: route_query.scope == SearchScope::All,
                            test_id: "global-search-scope-all",
                            class: "rounded-full border border-border px-3 py-1.5",
                            on_select: {
                                let query = route_query.clone();
                                move |()| update.call(SearchRouteQuery { scope: SearchScope::All, ..query.clone() })
                            },
                            Icon { icon: IconType::Terminal, class: "size-4" }
                            "All sessions"
                        }
                        CommandItem {
                            value: "needs-attention",
                            selected: route_query.scope == SearchScope::Attention,
                            test_id: "global-search-scope-attention",
                            class: "rounded-full border border-border px-3 py-1.5",
                            on_select: {
                                let query = route_query.clone();
                                move |()| update.call(SearchRouteQuery { scope: SearchScope::Attention, ..query.clone() })
                            },
                            Icon { icon: IconType::Bell, class: "size-4" }
                            "Needs attention"
                        }
                        if route_query.scope == SearchScope::All {
                            CommandItem {
                                value: "match-case",
                                selected: route_query.case_sensitive,
                                test_id: "global-search-case-sensitive",
                                class: "rounded-full border border-border px-3 py-1.5",
                                on_select: {
                                    let query = route_query.clone();
                                    move |()| update.call(SearchRouteQuery { case_sensitive: !query.case_sensitive, ..query.clone() })
                                },
                                Icon { icon: IconType::CaseSensitive, class: "size-4" }
                                "Match terminal case"
                            }
                        }
                    }
                }
            }
            div { class: "flex flex-1 flex-col gap-4 overflow-auto p-4",
                if route_query.scope == SearchScope::All {
                    content::ContentResults { route_query: route_query.clone() }
                }
                metadata::MetadataResults { route_query: route_query.clone() }
            }
        }
    }
}
