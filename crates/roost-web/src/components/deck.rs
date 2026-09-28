//! The terminal deck: the persistent surface that keeps every warm terminal
//! mounted and paints the folder's panes, strips, dividers and phone bar.
//! Rendered by the shell's `MainPane`; state lives in `roost_client_core::deck`.
//! Ports `apps/web/src/components/deck/*`.

pub mod deck_swipe;
pub mod deck_swipe_style;
pub mod inline_style;
pub mod pane_strip_drag;
pub mod terminal_deck_geometry;
pub mod terminal_deck_model;
pub mod terminal_deck_shortcuts;
