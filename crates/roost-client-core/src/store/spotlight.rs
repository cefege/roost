//! Spotlight: which session is floated, and how many panes the deck shows.
//!
//! Two pieces of state and one predicate, ported from
//! `apps/web/src/store/spotlight.ts` (15 lines). The pane count is published by
//! the deck each frame because the context menu gates its spotlight item on it:
//! floating one pane out of one is a no-op the menu should not offer.
//!
//! Depends on nothing but `store`.

use crate::store::Store;

/// The spotlight and the deck's pane count.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Spotlight {
    session_id: Option<String>,
    visible_pane_count: u32,
}

impl Spotlight {
    /// Nothing floated, no panes published yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The floated session, if one is.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// How many panes the deck shows.
    pub fn visible_pane_count(&self) -> u32 {
        self.visible_pane_count
    }
}

/// Float a session, or stop floating one.
///
/// Returns whether the store changed, so a caller that toggles does not have to
/// read the state back to decide whether to repaint.
pub fn set_spotlight_session_id(store: &mut Store, session_id: Option<String>) -> bool {
    if store.spotlight.session_id == session_id {
        return false;
    }
    store.spotlight.session_id = session_id;
    store.note_change();
    true
}

/// Stop floating whatever is floated.
pub fn clear_spotlight(store: &mut Store) -> bool {
    set_spotlight_session_id(store, None)
}

/// Whether `session_id` is the floated one.
pub fn is_spotlit(store: &Store, session_id: &str) -> bool {
    store.spotlight.session_id() == Some(session_id)
}

/// Publish the deck's pane count, once per frame.
pub fn set_visible_pane_count(store: &mut Store, count: u32) -> bool {
    if store.spotlight.visible_pane_count == count {
        return false;
    }
    store.spotlight.visible_pane_count = count;
    store.note_change();
    true
}
