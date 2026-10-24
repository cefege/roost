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
use libero::components::Pictogram;
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
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::router_state::use_navigate;
use libero::components::{Badge, Box, Button};
use libero::sx::sx;

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
    let panel_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("sm")
            .border_radius("xl")
            .border_width("1px")
            .border_color("var(--border)")
            .background_color("var(--card)")
            .padding("md")
            .color("var(--card-foreground)")
    });
    let empty_icon_sx = use_hook(|| {
        sx().width("1rem")
            .height("1rem")
            .color("var(--muted-foreground)")
    });
    let title_sx = use_hook(|| sx().margin("0").font_size("lg").font_weight("600"));
    let summary_sx = use_hook(|| sx().font_size("sm").font_weight("500"));
    let empty_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("sm")
            .padding_block("md")
    });
    let empty_title_sx = use_hook(|| sx().font_size("sm").font_weight("500"));
    let empty_supporting_sx = use_hook(|| sx().font_size("sm").color("var(--muted-foreground)"));
    let results_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("sm"));

    rsx! {
        Box { component: "section", sx: panel_sx.clone(),
            "aria-labelledby": "global-search-metadata-title",
            Box { component: "h2", id: "global-search-metadata-title", sx: title_sx.clone(),
                match route_query.scope {
                    SearchScope::Attention => "Agent attention",
                    SearchScope::All => "Session metadata",
                }
            }
            Box { component: "div", sx: summary_sx.clone(), role: "status", "aria-live": "polite", "aria-atomic": "true",
                {summary}
            }
            if rows.is_empty() {
                Box { component: "div", sx: empty_sx.clone(),
                    Box { component: "span", sx: empty_icon_sx.clone(),
                        Pictogram {
                            icon: match route_query.scope {
                                SearchScope::Attention => pictogram_icons_lucide::bell_off::outlined,
                                SearchScope::All => pictogram_icons_lucide::search_x::outlined,
                            },
                            width: "1rem",
                            height: "1rem",
                        }
                    }
                    Box { component: "span", sx: empty_title_sx.clone(), {empty_title(route_query.scope, &route_query.text)} }
                    Box { component: "span", sx: empty_supporting_sx.clone(), {empty_supporting(route_query.scope, &route_query.text)} }
                }
            } else {
                Box { component: "div", sx: results_sx.clone(), "data-testid": "global-search-results",
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
    let status_label = if document.available {
        "Available"
    } else {
        "Machine unavailable"
    };
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
    let row_title_text_sx = use_hook(|| {
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
    let status_sx = use_hook(|| {
        sx().display("flex")
            .flex_shrink("0")
            .align_items("center")
            .gap("sm")
            .white_space("nowrap")
            .font_size("xs")
            .color("var(--muted-foreground)")
    });
    rsx! {
        Button {
            variant: "standard",
            sx: row_button_sx.clone(),
            "data-testid": format!("global-search-result-{session_id}"),
            onclick: move |_| on_open.call(()),
            icon: rsx! { Box { component: "span", sx: row_icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::terminal::outlined, width: "1rem", height: "1rem" } } },
            Box { component: "span", sx: row_body_sx.clone(),
                Box { component: "span", sx: row_title_text_sx.clone(), "data-testid": format!("global-search-title-{session_id}"),
                    {document.display_title.clone()}
                }
                Box { component: "span", sx: row_details_sx.clone(),
                    Box { component: "span", {document.cwd.clone()} }
                    if !metadata.is_empty() {
                        Box { component: "span", {metadata} }
                    }
                    if let Some(message) = document.agent_message.clone() {
                        Box { component: "span", {message} }
                    }
                }
            }
            Box { component: "span", sx: status_sx.clone(),
                if let Some(label) = attention_label(document.agent_attention) {
                    span { "data-testid": format!("global-search-attention-{session_id}"), {label} }
                }
                Badge {
                    variant: "outlined",
                    color: if document.available { "success" } else { "error" },
                    title: status_label,
                    {status_label}
                }
                span { "data-testid": format!("global-search-availability-{session_id}"),
                    {availability}
                }
            }
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
}
