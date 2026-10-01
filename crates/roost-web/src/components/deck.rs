//! The terminal deck: the persistent surface that keeps every warm terminal
//! mounted and paints the folder's panes, strips, dividers and phone bar.
//! Rendered by the shell's `MainPane`; state lives in `roost_client_core::deck`.
//! Ports `apps/web/src/components/deck/*`.

pub mod arrange_menu;
pub mod deck_dom;
pub mod deck_swipe;
pub mod deck_swipe_style;
pub mod deck_swipe_touch;
pub mod inline_style;
pub mod mobile_deck_bar;
pub mod pane_divider;
pub mod pane_strip;
pub mod pane_strip_double_press;
pub mod pane_strip_drag;
pub mod pane_strip_gesture;
pub mod pane_tab;
pub mod pane_tab_hover_card;
pub mod pane_tab_list;
pub mod terminal_deck;
pub mod terminal_deck_chords;
pub mod terminal_deck_chrome;
pub mod terminal_deck_geometry;
pub mod terminal_deck_hooks;
pub mod terminal_deck_model;
pub mod terminal_deck_operations;
pub mod terminal_deck_shortcuts;
pub mod terminal_deck_spawn;
pub mod terminal_deck_spotlight;
pub mod terminal_deck_swipe;
pub mod terminal_deck_swipe_overlay;
pub mod workspace_tabs_menu;
pub mod workspace_tabs_sheet;
