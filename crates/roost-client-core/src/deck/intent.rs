//! What a deck can ask of the core: the gestures that reshape a folder's
//! arrangement, the spotlight, closing a tab, and the observation that keeps
//! the stored arrangement in step with the route. Raised by the web deck as
//! `ClientEvent::Deck`; applied by `handle_event`. Ports `apps/web/src/lib/deckOps.ts`
//! and the layout commits of `apps/web/src/components/deck/terminal-deck-{model,operations}.ts`.

mod close;
mod follow;

use roost_protocol::layout::document::LayoutDirection;

use super::route_selection::pane_focus_persists;
use crate::platform::KeyValueStore;
use crate::store::Store;
use crate::store::layout::{
    ArrangeKind, PaneLayout, arrange_layout, find_leaf, find_leaf_of_tab, focus_pane, move_tab,
    reorder_tab, select_tab, set_ratio, split_leaf,
};
use crate::store::spotlight::{clear_spotlight, set_spotlight_session_id, set_visible_pane_count};

/// The folder bucket an intent commits into, with its live membership in the
/// selector's order (oldest first). The host resolves both, because the
/// folder key needs the host's path codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeckFolder {
    /// The bucket key (`store::paths::folder_key_of`).
    pub folder_key: String,
    /// `store::selectors::live_session_ids_for_folder` for that key.
    pub live_session_ids: Vec<String>,
}

/// One deck intent.
#[derive(Debug, Clone, PartialEq)]
pub enum DeckIntent {
    /// The followed session, its folder, the live set or the painted pane
    /// count moved: seed the folder, pull the route's session into the focused
    /// pane, publish the pane count, and drop a spotlight that no longer applies.
    Observed {
        /// The followed session's folder, if the deck follows one.
        folder: Option<DeckFolder>,
        /// The session the deck follows.
        followed_session_id: Option<String>,
        /// Whether the host paints one pane.
        compact: bool,
        /// How many panes the deck paints this frame.
        visible_pane_count: u32,
    },
    /// A tab click or a swipe landing: select it and show it.
    SelectTab {
        folder: DeckFolder,
        session_id: String,
        compact: bool,
    },
    /// A pane body click: give it the keyboard and show its selected tab.
    FocusPane {
        folder: DeckFolder,
        pane_id: String,
        compact: bool,
    },
    /// A terminal the deck spawned is live: give the pane it was opened from
    /// the keyboard and show it; the next observation pulls it into that pane.
    OpenSpawned {
        folder: DeckFolder,
        pane_id: String,
        session_id: String,
        compact: bool,
    },
    /// A tab dragged to a new position within its strip.
    ReorderTabs {
        folder: DeckFolder,
        pane_id: String,
        ordered_ids: Vec<String>,
    },
    /// A tab dropped on another pane's centre.
    MoveTab {
        folder: DeckFolder,
        tab_id: String,
        to_pane_id: String,
    },
    /// A tab dropped on a pane edge, or a fresh session split beside a pane.
    SplitPane {
        folder: DeckFolder,
        pane_id: String,
        direction: LayoutDirection,
        tab_id: String,
        insert_first: bool,
    },
    /// A divider released at `ratio`.
    SetRatio {
        folder: DeckFolder,
        split_id: String,
        ratio: f64,
    },
    /// An arrange-menu preset or its shortcut.
    Arrange {
        folder: DeckFolder,
        kind: ArrangeKind,
        active_session_id: Option<String>,
    },
    /// Float the focused pane's tab, or put a floated one back.
    ToggleSpotlight { folder: DeckFolder },
    /// Put a floated pane back (scrim click, Escape).
    ClearSpotlight,
    /// A tab's ✕: hide it now, owe the kill after the undo window.
    CloseTab {
        folder: Option<DeckFolder>,
        session_id: String,
        active_session_id: Option<String>,
        labels: crate::store::pending_close::CloseLabels,
    },
    /// The undo card's Undo: restore the tab and the tiling it closed from.
    UndoClose { session_id: String },
}

impl DeckIntent {
    /// A short name for the incident log.
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Observed { .. } => "observed",
            Self::SelectTab { .. } => "select_tab",
            Self::FocusPane { .. } => "focus_pane",
            Self::OpenSpawned { .. } => "open_spawned",
            Self::ReorderTabs { .. } => "reorder_tabs",
            Self::MoveTab { .. } => "move_tab",
            Self::SplitPane { .. } => "split_pane",
            Self::SetRatio { .. } => "set_ratio",
            Self::Arrange { .. } => "arrange",
            Self::ToggleSpotlight { .. } => "toggle_spotlight",
            Self::ClearSpotlight => "clear_spotlight",
            Self::CloseTab { .. } => "close_tab",
            Self::UndoClose { .. } => "undo_close",
        }
    }
}

pub fn deck_tab_path(tab_id: &str) -> String {
    super::tab::DeckTab::parse(tab_id)
        .map(|tab| tab.path())
        .unwrap_or_else(|| format!("/s/{tab_id}"))
}

/// Apply one intent to the store.
pub(crate) fn apply_deck_intent(
    store: &mut Store,
    intent: &DeckIntent,
    storage: &dyn KeyValueStore,
    now_ms: u64,
) {
    tracing::debug!(target: "deck", intent = intent.kind_name(), "deck intent");
    match intent {
        DeckIntent::Observed {
            folder,
            followed_session_id,
            compact,
            visible_pane_count,
        } => {
            follow::observe(
                store,
                folder.as_ref(),
                followed_session_id.as_deref(),
                *compact,
                storage,
            );
            set_visible_pane_count(store, *visible_pane_count);
        }
        DeckIntent::SelectTab {
            folder,
            session_id,
            compact,
        } => {
            follow::select_session(store, folder, session_id, *compact, storage);
        }
        DeckIntent::FocusPane {
            folder,
            pane_id,
            compact,
        } => {
            let current = store.deck.resolve_for_edit(folder);
            if !pane_focus_persists(Some(&current), pane_id, *compact)
                || current.focused_pane_id == *pane_id
            {
                return;
            }
            let Some(selected) =
                find_leaf(&current.root, pane_id).map(|leaf| leaf.selected_tab.clone())
            else {
                return;
            };
            commit(store, folder, focus_pane(&current, pane_id), storage);
            if !selected.is_empty() {
                store.deck.request_navigation(deck_tab_path(&selected));
            }
        }
        DeckIntent::OpenSpawned {
            folder,
            pane_id,
            session_id,
            compact,
        } => {
            // The focus is committed over the arrangement WITHOUT the new
            // session: folding it in first would park it in the previously
            // focused pane, and the next resolve must land it in `pane_id`.
            let before = DeckFolder {
                folder_key: folder.folder_key.clone(),
                live_session_ids: folder
                    .live_session_ids
                    .iter()
                    .filter(|id| *id != session_id)
                    .cloned()
                    .collect(),
            };
            let current = store.deck.resolve_for_edit(&before);
            if pane_focus_persists(Some(&current), pane_id, *compact)
                && current.focused_pane_id != *pane_id
            {
                commit(store, &before, focus_pane(&current, pane_id), storage);
            }
            store.deck.request_navigation(deck_tab_path(session_id));
            store.note_change();
        }
        DeckIntent::ReorderTabs {
            folder,
            pane_id,
            ordered_ids,
        } => {
            let current = store.deck.resolve_for_edit(folder);
            commit(
                store,
                folder,
                reorder_tab(&current, pane_id, ordered_ids.clone()),
                storage,
            );
        }
        DeckIntent::MoveTab {
            folder,
            tab_id,
            to_pane_id,
        } => {
            let current = store.deck.resolve_for_edit(folder);
            commit(
                store,
                folder,
                move_tab(&current, tab_id, to_pane_id, None),
                storage,
            );
            store.deck.request_navigation(deck_tab_path(tab_id));
        }
        DeckIntent::SplitPane {
            folder,
            pane_id,
            direction,
            tab_id,
            insert_first,
        } => {
            let current = store.deck.resolve_for_edit(folder);
            let next = split_leaf(
                &current,
                pane_id,
                direction.clone(),
                tab_id,
                *insert_first,
                &mut store.deck.pane_ids,
            );
            commit(store, folder, next, storage);
            store.deck.request_navigation(deck_tab_path(tab_id));
        }
        DeckIntent::SetRatio {
            folder,
            split_id,
            ratio,
        } => {
            let current = store.deck.resolve_for_edit(folder);
            let next = PaneLayout {
                root: set_ratio(&current.root, split_id, *ratio),
                ..current
            };
            commit(store, folder, next, storage);
        }
        DeckIntent::Arrange {
            folder,
            kind,
            active_session_id,
        } => {
            clear_spotlight(store);
            let current = store.deck.resolve_for_edit(folder);
            let mut next = arrange_layout(
                *kind,
                &current,
                &folder.live_session_ids,
                &mut store.deck.pane_ids,
            );
            if let Some(active) = active_session_id
                && *kind != ArrangeKind::Balance
                && find_leaf_of_tab(&next.root, active).is_some()
            {
                next = select_tab(&next, active);
            }
            commit(store, folder, next, storage);
        }
        DeckIntent::ToggleSpotlight { folder } => {
            if store.spotlight.session_id().is_some() {
                clear_spotlight(store);
                return;
            }
            let current = store.deck.resolve_for_edit(folder);
            if let Some(leaf) = find_leaf(&current.root, &current.focused_pane_id)
                && !leaf.selected_tab.is_empty()
            {
                set_spotlight_session_id(store, Some(leaf.selected_tab.clone()));
            }
        }
        DeckIntent::ClearSpotlight => {
            clear_spotlight(store);
        }
        DeckIntent::CloseTab {
            folder,
            session_id,
            active_session_id,
            labels,
        } => {
            close::close_session(
                store,
                folder.as_ref(),
                session_id,
                active_session_id.as_deref(),
                labels,
                storage,
                now_ms,
            );
        }
        DeckIntent::UndoClose { session_id } => close::undo_close(store, session_id, storage),
    }
}

fn commit(store: &mut Store, folder: &DeckFolder, next: PaneLayout, storage: &dyn KeyValueStore) {
    store.deck.commit(&folder.folder_key, next, storage);
    store.note_change();
}
