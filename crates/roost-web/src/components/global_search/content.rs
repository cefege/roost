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
use libero::components::Pictogram;
use roost_client_core::search::global::GlobalSearchMatch;
use roost_client_core::store::navigation::NavigationSearchDocument;
use roost_client_core::store::sidebar::documents::store_navigation_documents;
use roost_web_terminal::find::hits::PreferredMatch;
use roost_web_terminal::find::intent::TerminalFindIntentOptions;

use crate::components::global_search::{driver, query::SearchRouteQuery};
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::{Pump, use_store};
use libero::components::{Box, Button, ButtonGroup};
use libero::sx::sx;

use crate::router_state::use_navigate;

mod summary;
pub use summary::{incomplete, join_matches, partial_line, summary};

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
    let panel_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("md")
            .border_radius("xl")
            .border_width("1px")
            .border_color("var(--border)")
            .background_color("var(--card)")
            .padding("md")
    });
    let incomplete_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("sm")
            .border_radius("lg")
            .border_width("1px")
            .border_color("var(--border)")
            .background_color("var(--muted)")
            .padding("sm")
    });
    let empty_icon_sx = use_hook(|| sx().width("1rem").height("1rem"));
    let section_title_sx = use_hook(|| sx().margin("0").font_size("lg").font_weight("600"));
    let summary_text_sx = use_hook(|| sx().font_size("sm"));
    let empty_message_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .align_items("flex-start")
            .gap("sm")
            .padding_block("md")
    });
    let empty_title_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("sm")
            .font_weight("500")
    });
    let supporting_sx = use_hook(|| {
        sx().margin("0")
            .font_size("sm")
            .color("var(--muted-foreground)")
    });
    let result_list_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("sm"));
    let small_text_sx = use_hook(|| sx().font_size("sm"));
    let small_emphasis_sx = use_hook(|| sx().font_size("sm").font_weight("500"));
    let pagination_sx = use_hook(|| sx().display("flex").justify_content("center"));

    rsx! {
        Box { component: "section", sx: panel_sx.clone(),
            role: "region",
            "aria-labelledby": "global-content-search-title",
            Box { component: "h2", id: "global-content-search-title", sx: section_title_sx.clone(),
                "Terminal content"
            }
            Box { component: "div", sx: summary_text_sx.clone(), role: "status", "aria-live": "polite", "aria-atomic": "true",
                "data-testid": "global-content-summary",
                {summary(&results, &rows, waiting, &route_query.text)}
            }
            if searching {
                if rows.is_empty() {
                    if !waiting {
                        Box { component: "div", sx: empty_message_sx.clone(),
                            Box { component: "div", sx: empty_title_sx.clone(),
                                Box { component: "span", sx: empty_icon_sx.clone(),
                                    Pictogram {
                                        icon: if results.error.is_some() { pictogram_icons_lucide::circle_alert::outlined } else { pictogram_icons_lucide::search_x::outlined },
                                        width: "1rem",
                                        height: "1rem",
                                    }
                                }
                                {if results.error.is_some() {
                                    "Terminal content search failed"
                                } else if incomplete {
                                    "No matches in the completed portion"
                                } else {
                                    "No terminal content matches"
                                }}
                            }
                            Box { component: "p", sx: supporting_sx.clone(),
                                {results.error.clone().unwrap_or_else(|| {
                                    if incomplete {
                                        "Some sessions or retained rows could not be searched.".to_owned()
                                    } else {
                                        "No retained terminal row contains this literal query.".to_owned()
                                    }
                                })}
                            }
                            if results.retryable {
                                Button {
                                    variant: "outlined",
                                    icon: rsx! { Pictogram { icon: pictogram_icons_lucide::refresh_cw::outlined, width: "1rem", height: "1rem" } },
                                    "data-testid": "global-content-retry",
                                    onclick: {
                                        let pump = pump.clone();
                                        move |_| driver::retry(&pump)
                                    },
                                    "Retry"
                                }
                            }
                        }
                    }
                } else {
                    Box { component: "div", sx: result_list_sx.clone(),
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
                    Box { component: "section", sx: incomplete_sx.clone(),
                        "data-testid": "global-content-incomplete",
                        role: if results.error.is_some() { "alert" } else { "status" },
                        "aria-live": if results.error.is_some() { "assertive" } else { "polite" },
                        Box { component: "span", sx: small_emphasis_sx.clone(), "Search incomplete" }
                        if results.next_cursor.is_some() {
                            Box { component: "span", sx: small_text_sx.clone(), "More retained terminal rows are available." }
                        }
                        if results.eligible_sessions > 0
                            && results.searched_sessions < results.eligible_sessions {
                            Box { component: "span", sx: small_text_sx.clone(), "data-testid": "global-content-unsearched",
                                "{unsearched} of {eligible} eligible {unsearched_noun} not searched."
                            }
                        }
                        if results.truncated && results.next_cursor.is_none() {
                            Box { component: "span", sx: small_text_sx.clone(),
                                "The bounded page ended before every retained row could be searched."
                            }
                        }
                        if missing > 0 {
                            Box { component: "span", sx: small_text_sx.clone(),
                                "{missing} {missing_noun} sessions no longer present in the current session list."
                            }
                        }
                        for partial in results.partials.iter() {
                            Box { component: "span", sx: small_text_sx.clone(),
                                "data-testid": format!("global-content-partial-{}", partial.session_id.as_str()),
                                {partial_line(partial, &documents)}
                            }
                        }
                    }
                }
                if results.next_cursor.is_some() && results.error.is_none() {
                    Box { component: "div", sx: pagination_sx.clone(), role: "group", "aria-label": "Terminal content pagination",
                        ButtonGroup {
                            Button {
                                variant: "outlined",
                                icon: rsx! { Pictogram { icon: pictogram_icons_lucide::chevron_down::outlined, width: "1rem", height: "1rem" } },
                                disabled: waiting,
                                "data-testid": "global-content-load-more",
                                onclick: {
                                    let pump = pump.clone();
                                    move |_| { driver::take_next_page(&pump); }
                                },
                                if waiting { "Loading…" } else { "Load more" }
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
    let row_button_sx = use_hook(|| {
        sx().display("flex")
            .width("100%")
            .height("auto")
            .align_items("flex-start")
            .justify_content("flex-start")
            .gap("md")
            .border_radius("md")
            .padding_block("sm")
            .padding_inline("md")
            .text_align("left")
    });
    let row_icon_sx = use_hook(|| {
        sx().width("1rem")
            .height("1rem")
            .flex_shrink("0")
            .margin_top("xs")
            .color("var(--muted-foreground)")
    });
    let row_body_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .flex("1")
            .flex_direction("column")
            .gap("xs")
    });
    let row_title_sx = use_hook(|| {
        sx().font_size("sm")
            .font_weight("500")
            .color("var(--foreground)")
    });
    let row_details_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("xs")
            .font_size("xs")
            .color("var(--muted-foreground)")
    });
    let preview_sx = use_hook(|| {
        sx().white_space("pre-wrap")
            .with("overflow-wrap", "anywhere")
    });
    let open_find_sx = use_hook(|| {
        sx().flex_shrink("0")
            .white_space("nowrap")
            .font_size("xs")
            .color("var(--muted-foreground)")
    });
    let Some(document) = row.document.clone() else {
        return rsx! {};
    };
    let session_id = document.session_id.clone();
    rsx! {
        Button {
            variant: "standard",
            sx: row_button_sx.clone(),
            "data-testid": format!("global-content-result-{session_id}-{index}"),
            onclick: move |_| on_open.call(()),
            icon: rsx! { Box { component: "span", sx: row_icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::file_search::outlined, width: "1rem", height: "1rem" } } },
            Box { component: "span", sx: row_body_sx.clone(),
                Box { component: "span", sx: row_title_sx.clone(), "data-testid": format!("global-content-title-{session_id}"),
                    {document.display_title.clone()}
                }
                Box { component: "span", sx: row_details_sx.clone(),
                    Box { component: "span", sx: preview_sx.clone(),
                        "data-testid": format!("global-content-preview-{session_id}-{index}"),
                        {row.candidate.preview.clone()}
                    }
                    Box { component: "span", "{document.cwd} · {document.worker_label}" }
                }
            }
            Box { component: "span", sx: open_find_sx.clone(), "Open find" }
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
