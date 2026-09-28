//! One render's worth of deck state, derived from the store in one borrow:
//! the followed session and its folder, the arrangement, the painted panes,
//! each session's slot, the parked sizes and the phone tab order. Read by
//! `terminal_deck` during render. Target-independent; ports the memos of
//! `apps/web/src/components/deck/terminal-deck-model.ts` over
//! `roost_client_core::deck::view`.

use std::collections::BTreeMap;

use roost_client_core::Store;
use roost_client_core::deck::{
    DeckFolder, DeckSize, DeckView, TerminalSessionSlot, deck_session_id, deck_view,
    mobile_tab_ids, park_size_by_session, slot_by_session, spotlight_pane, spotlight_rect,
};
use roost_client_core::store::{Session, WorkerPaths};
use roost_client_core::store::layout::{PaneLayout, PaneRect};
use roost_client_core::store::selectors::{live_session_ids_for_folder, session_by_id, session_folder_key};
use roost_protocol::wire::SessionStatus;

use super::terminal_deck_geometry::MOBILE_TERMINAL_STRIP_HEIGHT;
use crate::platform::worker_paths::short_worker_path;

/// What the component knows that the store does not.
#[derive(Debug, Clone, PartialEq)]
pub struct DeckInputs<'input> {
    /// The route's open session.
    pub active_session_id: Option<&'input str>,
    /// Whether the deck is the visible surface (no overlay route above it).
    pub surface_visible: bool,
    /// The session the deck last followed, kept while an overlay covers it.
    pub retained_session_id: Option<&'input str>,
    /// Whether the host paints one pane.
    pub compact: bool,
    /// The measured deck box.
    pub size: DeckSize,
    /// `--workbench-tab-strip-height`, resolved on the deck element.
    pub desktop_strip_height: f64,
    /// Divider ratios mid-drag, by split id.
    pub drag_ratios: &'input BTreeMap<String, f64>,
    /// The session a compact swipe is sliding in.
    pub swipe_neighbor_id: Option<&'input str>,
}

/// The deck for one render.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeckFrame {
    /// The session the deck follows.
    pub followed_session_id: Option<String>,
    /// Its folder bucket and that folder's live sessions.
    pub folder: Option<DeckFolder>,
    /// The folder's arrangement, reconciled against the live sessions.
    pub layout: Option<PaneLayout>,
    /// The painted panes and dividers.
    pub view: DeckView,
    /// Every open session, in store order.
    pub open_session_ids: Vec<String>,
    /// The floated pane's id.
    pub spotlight_pane_id: Option<String>,
    /// The floated card's box.
    pub spotlight_rect: Option<PaneRect>,
    /// Where each painted session sits.
    pub slots: BTreeMap<String, TerminalSessionSlot>,
    /// The box every pane's tabs park at.
    pub park_sizes: BTreeMap<String, DeckSize>,
    /// The phone's flat tab order.
    pub mobile_tabs: Vec<String>,
    /// The short folder a new terminal would open in.
    pub new_terminal_folder: String,
    /// The strip height above each pane.
    pub strip_height: f64,
}

/// The folder bucket `session` belongs to and that folder's live sessions,
/// as a deck intent commits into it.
pub fn deck_folder_for(store: &Store, paths: &dyn WorkerPaths, session: &Session) -> DeckFolder {
    let folder_key = session_folder_key(store, paths, session);
    let live_session_ids = live_session_ids_for_folder(store, paths, &folder_key);
    DeckFolder {
        folder_key,
        live_session_ids,
    }
}

/// Derive the frame.
pub fn deck_frame(store: &Store, paths: &dyn WorkerPaths, inputs: &DeckInputs<'_>) -> DeckFrame {
    let followed = deck_session_id(
        inputs.active_session_id,
        inputs.surface_visible,
        inputs.retained_session_id,
    );
    let followed_session = followed.as_deref().and_then(|id| session_by_id(store, id));
    let folder = followed_session.map(|session| deck_folder_for(store, paths, session));
    let new_terminal_folder = followed_session.map_or_else(String::new, |session| {
        let worker_os = store.workers.get(session.worker_fp.as_str()).map(|worker| worker.os.as_str());
        short_worker_path(worker_os, &session.cwd)
    });
    let layout = folder.as_ref().map(|folder| store.deck.resolve_layout(folder));
    let view = deck_view(
        layout.as_ref(),
        inputs.size,
        inputs.drag_ratios,
        inputs.compact,
        followed.as_deref(),
    );
    let spotlit = spotlight_pane(&view, store.spotlight.session_id(), inputs.compact);
    let card = spotlit.and_then(|_| spotlight_rect(inputs.size));
    let slots = slot_by_session(
        &view,
        spotlit.zip(card),
        inputs.swipe_neighbor_id,
        inputs.compact,
    );
    let strip_height = if inputs.compact {
        MOBILE_TERMINAL_STRIP_HEIGHT
    } else {
        inputs.desktop_strip_height
    };
    DeckFrame {
        open_session_ids: store
            .sessions
            .sessions()
            .values()
            .filter(|session| session.status == SessionStatus::Open)
            .map(|session| session.id.as_str().to_owned())
            .collect(),
        spotlight_pane_id: spotlit.map(|pane| pane.pane_id.clone()),
        spotlight_rect: card,
        park_sizes: park_size_by_session(&view, strip_height),
        mobile_tabs: mobile_tab_ids(layout.as_ref(), inputs.compact),
        slots,
        view,
        followed_session_id: followed,
        folder,
        layout,
        new_terminal_folder,
        strip_height,
    }
}
