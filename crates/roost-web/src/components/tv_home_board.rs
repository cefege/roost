//! The TV home route's session board, ordered by actionable agent state.
//! Documents come from the shared navigation projection so the board agrees
//! with search and the sidebar about titles, locations and attention facts.

use std::collections::BTreeSet;

use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;
use roost_client_core::store::navigation::query::attention_navigation_documents;
use roost_client_core::store::navigation::{NavigationSearchAttention, NavigationSearchDocument};
use roost_client_core::store::sidebar::documents::store_navigation_documents;
use roost_protocol::wire::SessionStatus;

use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::components::md::list_row::is_in_app_navigation_click;
use crate::components::md::{Card, CardVariant, Chip, EmptyState};
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::router_state::use_navigate;

/// Rank the board's four status bands; attention details within the first and
/// third bands are ordered by the shared navigation selector.
fn status_band(document: &NavigationSearchDocument) -> u8 {
    match document.agent_status.as_deref() {
        Some("blocked") => 0,
        Some("working") => 1,
        Some("done") => 2,
        _ => 3,
    }
}

fn needs_operator_input(document: &NavigationSearchDocument) -> bool {
    document.agent_attention == Some(NavigationSearchAttention::Blocked)
}

/// The open session cards in TV reading order.
#[must_use]
pub fn order_tv_home_documents(
    documents: &[NavigationSearchDocument],
) -> Vec<&NavigationSearchDocument> {
    let attention = attention_navigation_documents(documents);
    let attention_positions: std::collections::BTreeMap<&str, usize> = attention
        .iter()
        .enumerate()
        .map(|(index, document)| (document.session_id.as_str(), index))
        .collect();
    let mut ordered: Vec<&NavigationSearchDocument> = documents.iter().collect();
    ordered.sort_by(|left, right| {
        status_band(left)
            .cmp(&status_band(right))
            .then_with(|| {
                if status_band(left) == 0 || status_band(left) == 2 {
                    attention_positions
                        .get(left.session_id.as_str())
                        .cmp(&attention_positions.get(right.session_id.as_str()))
                } else {
                    right.agent_arrival.cmp(&left.agent_arrival)
                }
            })
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    ordered
}

/// Live board shown instead of the desktop landing only in TV mode.
#[component]
pub fn TvHomeBoard() -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let revision = pump.revision();
    let documents = {
        let _ = revision.read();
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let open_sessions: BTreeSet<String> = store
            .sessions
            .sessions()
            .values()
            .filter(|session| session.status == SessionStatus::Open)
            .map(|session| session.id.as_str().to_owned())
            .collect();
        store_navigation_documents(
            store,
            &BrowserWorkerPaths,
            i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX),
        )
        .into_iter()
        .filter(|document| open_sessions.contains(&document.session_id))
        .collect::<Vec<_>>()
    };
    let sessions: Vec<_> = order_tv_home_documents(&documents)
        .into_iter()
        .map(|document| {
            let href = crate::routes::session_href(&document.session_id);
            (document, href)
        })
        .collect();
    rsx! {
        main { class: "tv-home-board", "data-testid": "tv-home-board", aria_label: "Open sessions",
            if sessions.is_empty() {
                EmptyState {
                    icon: "terminal",
                    title: "No open sessions",
                    supporting: Some("Open a workspace to see its sessions here.".to_owned()),
                    action: None,
                }
            } else {
                for (document, href) in sessions {
                    a {
                        class: "tv-home-board__link",
                        href: href.clone(),
                        "data-testid": "tv-home-session-{document.session_id}",
                        "data-unseen": document.agent_unseen,
                        "data-needs-input": needs_operator_input(document),
                        aria_label: "{document.display_title}, {document.worker_label}, {document.cwd}",
                        onclick: move |event: MouseEvent| {
                            if !is_in_app_navigation_click(
                                event.trigger_button() == Some(MouseButton::Primary),
                                event.modifiers(),
                            ) {
                                return;
                            }
                            event.prevent_default();
                            navigate.call(href.clone());
                        },
                        Card {
                            title: Some(document.display_title.clone()),
                            supporting: Some(format!("{} · {}", document.worker_label, document.cwd)),
                            variant: CardVariant::Elevated,
                            class: Some(if document.agent_unseen || needs_operator_input(document) { "tv-home-board__card tv-home-board__card--unseen" } else { "tv-home-board__card" }.to_owned()),
                            test_id: Some(format!("tv-home-card-{}", document.session_id)),
                            div { class: "tv-home-board__status",
                                if let Some(attention) = document.agent_attention {
                                    span { class: "tv-home-board__attention", {match attention {
                                        NavigationSearchAttention::Blocked => "Needs input",
                                        NavigationSearchAttention::Done => "Unseen completion",
                                    }}}
                                }
                                if matches!(document.agent_status.as_deref(), Some("blocked" | "working" | "done" | "idle")) {
                                    AgentStatusIndicator { session_id: document.session_id.clone() }
                                } else {
                                    Chip {
                                        label: "No agent status".to_owned(),
                                        icon: Some("info".to_owned()),
                                        selected: None,
                                        onclick: None,
                                        title: None,
                                        test_id: Some(format!("tv-home-status-{}", document.session_id)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_are_ordered_by_status_band_then_shared_attention_order() {
        let documents = vec![
            document("rest", "idle", None, false, 0),
            document(
                "done-seen",
                "done",
                Some(NavigationSearchAttention::Done),
                false,
                8,
            ),
            document("working", "working", None, false, 3),
            document(
                "blocked-seen",
                "blocked",
                Some(NavigationSearchAttention::Blocked),
                false,
                9,
            ),
            document(
                "done-unseen",
                "done",
                Some(NavigationSearchAttention::Done),
                true,
                2,
            ),
            document(
                "blocked-unseen",
                "blocked",
                Some(NavigationSearchAttention::Blocked),
                true,
                1,
            ),
        ];
        let ids: Vec<_> = order_tv_home_documents(&documents)
            .iter()
            .map(|document| document.session_id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "blocked-unseen",
                "blocked-seen",
                "working",
                "done-unseen",
                "done-seen",
                "rest"
            ]
        );
        assert!(order_tv_home_documents(&[]).is_empty());
    }

    fn document(
        session_id: &str,
        status: &str,
        attention: Option<NavigationSearchAttention>,
        unseen: bool,
        arrival: u64,
    ) -> NavigationSearchDocument {
        NavigationSearchDocument {
            session_id: session_id.to_owned(),
            href: String::new(),
            display_title: String::new(),
            custom_title: None,
            terminal_title: None,
            cwd: String::new(),
            spawn_cwd: None,
            workspace_id: None,
            workspace_name: None,
            folder_key: String::new(),
            worker_label: String::new(),
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
            agent_status: Some(status.to_owned()),
            agent_attention: attention,
            agent_unseen: unseen,
            agent_id: None,
            agent_message: None,
            agent_updated_at_ms: None,
            agent_arrival: arrival,
            client_only: false,
        }
    }
}
