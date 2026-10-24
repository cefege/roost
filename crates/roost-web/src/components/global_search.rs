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
use libero::components::{Box, Button, Pictogram, TextField};
use libero::sx::sx;

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
    let page_sx = use_hook(|| {
        sx().display("flex")
            .flex("1")
            .flex_direction("column")
            .overflow("hidden")
    });
    let text_field_sx = use_hook(|| {
        sx().border_radius("md")
            .border_width("1px")
            .border_color("var(--border)")
            .background_color("var(--background)")
            .padding_inline("md")
    });
    let scope_button_sx = use_hook(|| {
        sx().border_radius("9999px")
            .border_width("1px")
            .border_color("var(--border)")
            .padding_inline("md")
            .padding_block("xs")
    });
    let scope_icon_sx = use_hook(|| sx().width("1rem").height("1rem"));
    let search_header_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("sm")
            .padding("md")
    });
    let search_title_sx = use_hook(|| sx().margin("0").font_size("xxl").font_weight("600"));
    let search_intro_sx = use_hook(|| {
        sx().margin("0")
            .font_size("sm")
            .color("var(--muted-foreground)")
    });
    let search_controls_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("sm")
            .color("var(--foreground)")
    });
    let search_scope_group_sx = use_hook(|| sx().display("flex").flex_wrap("wrap").gap("sm"));
    let search_results_sx = use_hook(|| {
        sx().display("flex")
            .flex("1")
            .flex_direction("column")
            .gap("md")
            .overflow("auto")
            .padding("md")
    });
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
        Box { component: "section", sx: page_sx.clone(),
            "data-testid": "global-search-page",
            "aria-labelledby": "global-search-title",
            Box { component: "div", sx: search_header_sx.clone(),
                Box { component: "h1", id: "global-search-title", sx: search_title_sx.clone(),
                    "Search sessions"
                }
                Box { component: "p", sx: search_intro_sx.clone(),
                    "Find sessions by metadata and search retained terminal content across every machine."
                }
                Box { component: "div", sx: search_controls_sx.clone(),
                    TextField {
                        value: route_query.text.clone(),
                        aria_label: route_query.search_label().to_owned(),
                        placeholder: match route_query.scope {
                            SearchScope::All => "Title, path, or terminal text…",
                            SearchScope::Attention => "Title, path, agent status…",
                        },
                        id: "global-search-input",
                        "data-testid": "global-search-input",
                        attributes: vec![Attribute::new("autofocus", true, None, false)],
                        sx: text_field_sx.clone(),
                        oninput: {
                            let query = route_query.clone();
                            move |text: String| update.call(query.with_text(&text))
                        },
                    }
                    Box { component: "div", role: "group", "aria-label": "Search scope",
                        sx: search_scope_group_sx.clone(),
                        Button {
                            selected: route_query.scope == SearchScope::All,
                            "data-testid": "global-search-scope-all",
                            sx: scope_button_sx.clone(),
                            onclick: {
                                let query = route_query.clone();
                                move |_| update.call(SearchRouteQuery { scope: SearchScope::All, ..query.clone() })
                            },
                            icon: rsx! { Box { component: "span", sx: scope_icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::terminal::outlined, width: "1rem", height: "1rem" } } },
                            "All sessions"
                        }
                        Button {
                            selected: route_query.scope == SearchScope::Attention,
                            "data-testid": "global-search-scope-attention",
                            sx: scope_button_sx.clone(),
                            onclick: {
                                let query = route_query.clone();
                                move |_| update.call(SearchRouteQuery { scope: SearchScope::Attention, ..query.clone() })
                            },
                            icon: rsx! { Box { component: "span", sx: scope_icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::bell::outlined, width: "1rem", height: "1rem" } } },
                            "Needs attention"
                        }
                        if route_query.scope == SearchScope::All {
                            Button {
                                selected: route_query.case_sensitive,
                                "data-testid": "global-search-case-sensitive",
                                sx: scope_button_sx.clone(),
                                onclick: {
                                    let query = route_query.clone();
                                    move |_| update.call(SearchRouteQuery { case_sensitive: !query.case_sensitive, ..query.clone() })
                                },
                                icon: rsx! { Box { component: "span", sx: scope_icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::case_sensitive::outlined, width: "1rem", height: "1rem" } } },
                                "Match terminal case"
                            }
                        }
                    }
                }
            }
            Box { component: "div", sx: search_results_sx.clone(),
                if route_query.scope == SearchScope::All {
                    content::ContentResults { route_query: route_query.clone() }
                }
                metadata::MetadataResults { route_query: route_query.clone() }
            }
        }
    }
}
