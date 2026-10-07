//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "backfill_demand_waves.rs"]
mod backfill_demand_waves;
#[path = "backfill_direct_history.rs"]
mod backfill_direct_history;
#[path = "backfill_gap_paging.rs"]
mod backfill_gap_paging;
#[path = "block_placeholder.rs"]
mod block_placeholder;
#[path = "cell_row_painting.rs"]
mod cell_row_painting;
#[path = "cell_row_style_cache.rs"]
mod cell_row_style_cache;
#[path = "echo_overlay.rs"]
mod echo_overlay;
#[path = "echo_painter.rs"]
mod echo_painter;
#[path = "find_chain_epoch_seed.rs"]
mod find_chain_epoch_seed;
#[path = "find_controller_epoch.rs"]
mod find_controller_epoch;
#[path = "find_controller_paging.rs"]
mod find_controller_paging;
#[path = "find_intent.rs"]
mod find_intent;
#[path = "grid_geometry.rs"]
mod grid_geometry;
#[path = "history_page_placement.rs"]
mod history_page_placement;
#[path = "input_controller.rs"]
mod input_controller;
#[path = "kitty_functional_keys.rs"]
mod kitty_functional_keys;
#[path = "kitty_keyboard.rs"]
mod kitty_keyboard;
#[path = "link_target_classification.rs"]
mod link_target_classification;
#[path = "mouse_forwarding.rs"]
mod mouse_forwarding;
#[path = "mouse_pane.rs"]
mod mouse_pane;
#[path = "mouse_reporting.rs"]
mod mouse_reporting;
#[path = "presentation_snapshot.rs"]
mod presentation_snapshot;
#[path = "reader_intent_transitions.rs"]
mod reader_intent_transitions;
#[path = "render_append.rs"]
mod render_append;
#[path = "render_append_frames.rs"]
mod render_append_frames;
#[path = "render_append_holds.rs"]
mod render_append_holds;
#[path = "render_find_park.rs"]
mod render_find_park;
#[path = "render_geometry.rs"]
mod render_geometry;
#[path = "render_geometry_hit.rs"]
mod render_geometry_hit;
#[path = "render_held_window.rs"]
mod render_held_window;
#[path = "render_history.rs"]
mod render_history;
#[path = "render_history_checkpoint.rs"]
mod render_history_checkpoint;
#[path = "render_history_repair.rs"]
mod render_history_repair;
#[path = "render_presentation_controller.rs"]
mod render_presentation_controller;
#[path = "render_presentation_state.rs"]
mod render_presentation_state;
#[path = "render_reader_live.rs"]
mod render_reader_live;
#[path = "render_reader_park.rs"]
mod render_reader_park;
#[path = "render_reconcile_diff.rs"]
mod render_reconcile_diff;
#[path = "render_reconcile_notify.rs"]
mod render_reconcile_notify;
#[path = "render_row_links.rs"]
mod render_row_links;
#[path = "render_row_wide.rs"]
mod render_row_wide;
#[path = "render_scheduler.rs"]
mod render_scheduler;
#[path = "render_scheduler_activation.rs"]
mod render_scheduler_activation;
#[path = "render_scheduler_bounds.rs"]
mod render_scheduler_bounds;
#[path = "render_scheduler_cursor_poll.rs"]
mod render_scheduler_cursor_poll;
#[path = "render_scheduler_gates.rs"]
mod render_scheduler_gates;
#[path = "render_scroll_settle.rs"]
mod render_scroll_settle;
#[path = "sched_reader_scroll.rs"]
mod sched_reader_scroll;
#[path = "sched_startup_progress.rs"]
mod sched_startup_progress;
#[path = "selection_guard.rs"]
mod selection_guard;
#[path = "terminal_input.rs"]
mod terminal_input;
#[path = "terminal_links.rs"]
mod terminal_links;
#[path = "terminal_links_armed_hold.rs"]
mod terminal_links_armed_hold;
#[path = "terminal_links_attachment.rs"]
mod terminal_links_attachment;
#[path = "terminal_links_file_paths.rs"]
mod terminal_links_file_paths;
#[path = "terminal_links_gestures.rs"]
mod terminal_links_gestures;
#[path = "terminal_links_precedence.rs"]
mod terminal_links_precedence;
