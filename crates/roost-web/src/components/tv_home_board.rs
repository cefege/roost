//! The TV home route's open terminal sessions.
//! Documents come from the shared navigation projection.

use std::collections::BTreeSet;

use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;
use roost_client_core::store::navigation::NavigationSearchDocument;
use roost_client_core::store::sidebar::documents::store_navigation_documents;
use roost_protocol::wire::SessionStatus;

use crate::components::ui::list::is_in_app_navigation_click;
use crate::components::ui::{Card, CardDescription, CardHeader, CardTitle, EmptyState, IconType};
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::router_state::use_navigate;

/// The open session cards in TV reading order.
#[must_use]
pub fn order_tv_home_documents(
    documents: &[NavigationSearchDocument],
) -> Vec<&NavigationSearchDocument> {
    let mut ordered: Vec<&NavigationSearchDocument> = documents.iter().collect();
    ordered.sort_by(|left, right| left.session_id.cmp(&right.session_id));
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
        main { class: "flex min-h-0 min-w-0 flex-col overflow-auto p-5", "data-testid": "tv-home-board", aria_label: "Open sessions",
            if sessions.is_empty() {
                EmptyState {
                    icon: IconType::Terminal,
                    title: "No open sessions",
                    supporting: Some("Open a workspace to see its sessions here.".to_owned()),
                    action: None,
                }
            } else {
                for (document, href) in sessions {
                    a {
                        class: "block min-w-0 no-underline [color:inherit]",
                        href: href.clone(),
                        "data-testid": "tv-home-session-{document.session_id}",
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
                            class: Some("min-h-40 justify-center".to_owned()),
                            "data-testid": "tv-home-card-{document.session_id}",
                            CardHeader {
                                CardTitle { {document.display_title.clone()} }
                                CardDescription { {format!("{} · {}", document.worker_label, document.cwd)} }
                            }
                        }
                    }
                }
            }
        }
    }
}
