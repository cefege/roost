//! Closing a deck tab and undoing it: land the view on the tab the post-close
//! arrangement will actually show BEFORE the kill round-trips, and keep the
//! pre-close tiling for the undo. Called by `deck::intent`. Ports
//! `closeSessionOp` from `apps/web/src/lib/deckOps.ts` and the deck half of
//! `siblingOrHomeHref` (`apps/web/src/lib/closeSession.ts`).

use super::{DeckFolder, commit, session_path};
use crate::platform::KeyValueStore;
use crate::store::Store;
use crate::store::layout::{PaneLayout, close_tab, find_leaf};
use crate::store::optimistic_spawn::abort_optimistic_spawn;
use crate::store::pending_close::{CloseLabels, schedule_close, undo_one};

use crate::deck::state::CloseUndo;

/// Where the route goes when the viewed tab closes: the tab `after` shows in
/// its focused pane, else the newest other live session in the folder, else
/// home. Reusing `close_tab`'s own focus pick is what keeps the URL and the
/// deck from disagreeing about what is painted.
fn close_destination(
    after: Option<&PaneLayout>,
    folder: Option<&DeckFolder>,
    closing_session_id: &str,
) -> String {
    let shown = after
        .and_then(|layout| find_leaf(&layout.root, &layout.focused_pane_id))
        .map(|leaf| leaf.selected_tab.as_str())
        .filter(|tab| !tab.is_empty());
    if let Some(tab) = shown {
        return session_path(tab);
    }
    folder
        .and_then(|folder| {
            folder
                .live_session_ids
                .iter()
                .rev()
                .find(|id| id.as_str() != closing_session_id)
        })
        .map_or_else(|| "/".to_owned(), |sibling| session_path(sibling))
}

pub(super) fn close_session(
    store: &mut Store,
    folder: Option<&DeckFolder>,
    session_id: &str,
    active_session_id: Option<&str>,
    labels: &CloseLabels,
    storage: &dyn KeyValueStore,
    now_ms: u64,
) {
    // No PTY yet: drop the placeholder and its tab. The spawn that lands later
    // sees the abort and reaps itself, so no orphan survives.
    if store.spawns.is_pending(session_id) {
        if let Some(ticket) = store.spawns.ticket_for(session_id) {
            abort_optimistic_spawn(store, &ticket);
        }
        if let Some(folder) = folder {
            let current = store.deck.resolve_for_edit(folder);
            commit(store, folder, close_tab(&current, session_id), storage);
        }
        return;
    }
    let viewed = active_session_id == Some(session_id);
    let before = folder.map(|folder| store.deck.resolve_for_edit(folder));
    let after = before.as_ref().map(|layout| close_tab(layout, session_id));
    let destination = close_destination(after.as_ref(), folder, session_id);
    schedule_close(store, session_id, labels.clone(), now_ms);
    tracing::info!(target: "deck", session_id, viewed, "deck tab closing");
    if let (Some(folder), Some(before), Some(after)) = (folder, before, after) {
        store.deck.close_undo.insert(
            session_id.to_owned(),
            CloseUndo {
                folder_key: folder.folder_key.clone(),
                before,
                was_viewed: viewed,
            },
        );
        commit(store, folder, after, storage);
    }
    if viewed {
        store.deck.request_navigation(destination);
    }
}

pub(super) fn undo_close(store: &mut Store, session_id: &str, storage: &dyn KeyValueStore) {
    if undo_one(store, session_id).is_none() {
        return;
    }
    // Un-hidden first (above), then the tiling: the restore is the last write,
    // so the reconcile that re-admits the row cannot re-place it.
    let Some(undo) = store.deck.close_undo.remove(session_id) else {
        return;
    };
    tracing::info!(target: "deck", session_id, "deck tab close undone");
    store.deck.commit(&undo.folder_key, undo.before, storage);
    store.note_change();
    if undo.was_viewed {
        store.deck.request_navigation(session_path(session_id));
    }
}
