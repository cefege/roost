//! The terminal deck's state and derivations: the stored arrangement per
//! folder, the intents a deck raises against it, and the per-frame view a host
//! paints. Called by `handle_event` (intents) and by the web deck component
//! (views). Depends on `store::layout` for the tree and its edits. Ports
//! `apps/web/src/components/deck/terminal-deck-model.ts`, the layout half of
//! `terminal-deck-operations.ts`, and `apps/web/src/lib/{deckOps,deckRouteSelection,deckWarmSet,deckTabBadge}.ts`.

pub mod intent;
pub mod route_selection;
pub mod spawn;
pub mod state;
pub mod tab;
pub mod tab_badge;
pub mod view;
pub mod warm_set;
pub use intent::{DeckFolder, DeckIntent, deck_tab_path};
pub use route_selection::{
    SessionSelection, pane_focus_persists, route_selection_commit, session_selection,
};
pub use spawn::DeckSpawn;
pub use state::{DeckNavigation, DeckPaneIds, DeckState};
pub use tab::{DeckTab, agent_tab_id};
pub use tab_badge::{DeckTabBadge, deck_tab_badge};
pub use view::{
    DeckSize, DeckView, TerminalSessionSlot, deck_session_id, deck_view, mobile_tab_ids,
    mounted_session_ids, park_size_by_session, slot_by_session, spotlight_pane, spotlight_rect,
};
pub use warm_set::{DECK_WARM_LIMIT, WarmSet};
