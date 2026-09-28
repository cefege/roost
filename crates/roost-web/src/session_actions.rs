//! What a session close shows and where it lands: the three undo-card labels
//! every close path (deck tab ✕, sidebar ✕/swipe, "Close terminal") computes
//! identically. Ports `closeLabelsFor` from `apps/web/src/lib/closeSession.ts`
//! (its `siblingOrHomeHref` is `route_session::sibling_or_home_href`); read by
//! the sidebar rows and the deck, which hand the labels to `DeckIntent::CloseTab`.

use roost_client_core::Store;
use roost_client_core::store::CloseLabels;
use roost_client_core::store::Session;
use roost_client_core::store::sidebar::folder_groups::folder_display_name;
use roost_client_core::store::sidebar::format::short_server_label;

use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::session_naming::session_title;

/// How much of a fingerprint names a machine that has no label yet.
const FP_LABEL_CHARS: usize = 6;

/// The undo card's labels for closing `session`.
pub fn close_labels_for(store: &Store, session: &Session) -> CloseLabels {
    let worker_fp = session.worker_fp.as_str();
    let label = store
        .workers
        .get(worker_fp)
        .map(|worker| worker.label.clone())
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| worker_fp.chars().take(FP_LABEL_CHARS).collect());
    CloseLabels {
        terminal_name: session_title(store, session),
        folder: folder_display_name(store, &BrowserWorkerPaths, session),
        server: short_server_label(&label),
    }
}
