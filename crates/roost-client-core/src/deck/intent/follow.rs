//! The deck following the route: remembering which folder is observed,
//! dropping a stale spotlight, seeding and route-committing the arrangement,
//! and selecting a session tab (navigate-only in compact). Called by
//! `deck::intent`. Ports the route-follow and `selectSessionOp` halves of
//! `apps/web/src/lib/deckOps.ts` and `terminal-deck-operations.ts`.

use super::{DeckFolder, commit, deck_tab_path};
use crate::deck::route_selection::{SessionSelection, route_selection_commit, session_selection};
use crate::platform::KeyValueStore;
use crate::store::Store;
use crate::store::layout::{find_leaf_of_tab, select_tab};
use crate::store::spotlight::{clear_spotlight, set_spotlight_session_id};

pub(super) fn observe(
    store: &mut Store,
    folder: Option<&DeckFolder>,
    followed_session_id: Option<&str>,
    compact: bool,
    storage: &dyn KeyValueStore,
) {
    let folder_key = folder.map(|folder| folder.folder_key.clone());
    let previous = store.deck.observed_folder.replace(folder_key.clone());
    if previous.is_some_and(|previous| previous != folder_key) {
        clear_spotlight(store);
    }
    let stale_spotlight = store.spotlight.session_id().is_some_and(|tab_id| {
        if let Some(conversation_id) = tab_id.strip_prefix("agent:") {
            !store.agent_chat.conversations.contains_key(conversation_id)
                || store.pending_closes.contains(tab_id)
        } else {
            crate::store::selectors::session_by_id(store, tab_id)
                .is_none_or(|session| session.status != roost_protocol::wire::SessionStatus::Open)
        }
    });
    if stale_spotlight {
        clear_spotlight(store);
    }
    let pending = &store.pending_closes;
    store
        .deck
        .close_undo
        .retain(|session_id, _| pending.contains(session_id));
    let Some(folder) = folder else {
        return;
    };
    let seeded = store.deck.records.stored(&folder.folder_key).is_some();
    let current = store.deck.resolve_for_edit(folder);
    if !seeded {
        store.note_change();
    }
    if let Some(followed) = followed_session_id
        && let Some(next) = route_selection_commit(&current, followed, compact)
    {
        commit(store, folder, next, storage);
    }
}

pub(super) fn select_session(
    store: &mut Store,
    folder: &DeckFolder,
    session_id: &str,
    compact: bool,
    storage: &dyn KeyValueStore,
) {
    let current = store.deck.resolve_for_edit(folder);
    if session_selection(Some(&current), compact) == SessionSelection::NavigateOnly {
        store.deck.request_navigation(deck_tab_path(session_id));
        store.note_change();
        return;
    }
    // The floated pane is captured BEFORE the commit: a tab swapped inside a
    // floated card keeps the card up, so the spotlight follows the new tab.
    let spotlit_pane_id = store
        .spotlight
        .session_id()
        .filter(|_| !compact)
        .and_then(|spotlit| {
            find_leaf_of_tab(&current.root, spotlit).filter(|leaf| leaf.selected_tab == spotlit)
        })
        .map(|leaf| leaf.pane_id.clone());
    let next = select_tab(&current, session_id);
    let lands_in_spotlit = spotlit_pane_id.is_some_and(|pane_id| {
        find_leaf_of_tab(&next.root, session_id).is_some_and(|leaf| leaf.pane_id == pane_id)
    });
    commit(store, folder, next, storage);
    store.deck.request_navigation(deck_tab_path(session_id));
    if lands_in_spotlit {
        set_spotlight_session_id(store, Some(session_id.to_owned()));
    }
}
