//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "agent_attention.rs"]
mod agent_attention;
#[path = "agent_attention_count.rs"]
mod agent_attention_count;
#[path = "agent_notification_scheduler.rs"]
mod agent_notification_scheduler;
#[path = "carrier_layer.rs"]
mod carrier_layer;
#[path = "composer_slot_claim.rs"]
mod composer_slot_claim;
#[path = "connection_banner.rs"]
mod connection_banner;
#[path = "dead_route_safety_net.rs"]
mod dead_route_safety_net;
#[path = "deck_geometry.rs"]
mod deck_geometry;
#[path = "deck_store_subscription.rs"]
mod deck_store_subscription;
#[path = "deck_swipe.rs"]
mod deck_swipe;
#[path = "deck_swipe_touch.rs"]
mod deck_swipe_touch;
#[path = "deploy_dialog_state.rs"]
mod deploy_dialog_state;
#[path = "design_catalog.rs"]
mod design_catalog;
#[path = "directional_input.rs"]
mod directional_input;
#[path = "file_route_round_trip.rs"]
mod file_route_round_trip;
#[path = "global_search_page.rs"]
mod global_search_page;
#[path = "keyboard_shortcuts.rs"]
mod keyboard_shortcuts;
#[path = "md_dialog.rs"]
mod md_dialog;
#[path = "md_primitives.rs"]
mod md_primitives;
#[path = "md_select.rs"]
mod md_select;
#[path = "mic_unmount_lifecycle.rs"]
mod mic_unmount_lifecycle;
#[path = "notification_dock_lift.rs"]
mod notification_dock_lift;
#[path = "notify_target_ring.rs"]
mod notify_target_ring;
#[path = "pad_bindings.rs"]
mod pad_bindings;
#[path = "pad_folders.rs"]
mod pad_folders;
#[path = "pad_keypad_focus.rs"]
mod pad_keypad_focus;
#[path = "pad_mapper.rs"]
mod pad_mapper;
#[path = "pad_poll_reading.rs"]
mod pad_poll_reading;
#[path = "pad_router.rs"]
mod pad_router;
#[path = "pad_router_travel.rs"]
mod pad_router_travel;
#[path = "pairing_gate.rs"]
mod pairing_gate;
#[path = "palette_actions.rs"]
mod palette_actions;
#[path = "palette_overlay_lifecycle.rs"]
mod palette_overlay_lifecycle;
#[path = "pane_drawer_mount.rs"]
mod pane_drawer_mount;
#[path = "peer_carrier_attempts.rs"]
mod peer_carrier_attempts;
#[path = "perf_counters.rs"]
mod perf_counters;
#[path = "phase_marks.rs"]
mod phase_marks;
#[path = "pump_carrier_environment.rs"]
mod pump_carrier_environment;
#[path = "queue_task_dialog_mount.rs"]
mod queue_task_dialog_mount;
#[path = "resize_drag.rs"]
mod resize_drag;
#[path = "route_session.rs"]
mod route_session;
#[path = "route_surfaces.rs"]
mod route_surfaces;
#[path = "router_popstate_ownership.rs"]
mod router_popstate_ownership;
#[path = "routes.rs"]
mod routes;
#[path = "shell_metrics.rs"]
mod shell_metrics;
#[path = "shell_motion.rs"]
mod shell_motion;
#[path = "sidebar_logic.rs"]
mod sidebar_logic;
#[path = "spatial_navigation.rs"]
mod spatial_navigation;
#[path = "store_write_subscription.rs"]
mod store_write_subscription;
#[path = "terminal_deck_model.rs"]
mod terminal_deck_model;
#[path = "terminal_dom_repair.rs"]
mod terminal_dom_repair;
#[path = "terminal_file_link.rs"]
mod terminal_file_link;
#[path = "terminal_find_bar.rs"]
mod terminal_find_bar;
#[path = "terminal_input_echo.rs"]
mod terminal_input_echo;
#[path = "terminal_pane_presentation.rs"]
mod terminal_pane_presentation;
#[path = "terminal_pane_registry.rs"]
mod terminal_pane_registry;
#[path = "terminal_pane_rules.rs"]
mod terminal_pane_rules;
#[path = "terminal_viewport_publication.rs"]
mod terminal_viewport_publication;
#[path = "theme_engine.rs"]
mod theme_engine;
#[path = "theme_registry.rs"]
mod theme_registry;
#[path = "tv_modality.rs"]
mod tv_modality;
#[path = "ui_bridge_apply.rs"]
mod ui_bridge_apply;
#[path = "ui_bridge_report.rs"]
mod ui_bridge_report;
#[path = "undo_close_card.rs"]
mod undo_close_card;
#[path = "voice_draft_settle.rs"]
mod voice_draft_settle;
#[path = "worker_paths.rs"]
mod worker_paths;
