//! The terminal pane surface: `CellTerminal` (one canonical cell-grid pane),
//! `TerminalCard` (its tab-grid card), `TerminalTransportIndicator`, the
//! session-keyed `pane_registry` that resolves a session to its mounted
//! renderer, and the native state machines the imperative `pane_mount` drives.
//! Ports `apps/web/src/components/terminal/`.

pub mod card_swipe;
pub mod cell_terminal;
pub mod document_lifecycle;
pub mod dom;
pub mod dom_repair;
pub mod frame_feed;
pub mod offline_watch;
pub mod pane_echo_feedback;
#[cfg(feature = "smoke")]
pub mod pane_faults;
pub mod pane_handle;
pub mod pane_input;
#[cfg(target_arch = "wasm32")]
pub mod pane_mount;
pub mod pane_registry;
pub mod pane_state;
pub mod pane_status;
pub mod pane_surface;
pub mod startup_overlay_state;
pub mod terminal_card;
pub mod terminal_offline_notice;
pub mod terminal_paste_guard;
pub mod terminal_startup_overlay;
pub mod terminal_transport_indicator;
pub mod viewport_publication;
