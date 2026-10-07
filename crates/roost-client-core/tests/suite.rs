//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "agent_status_ordering.rs"]
mod agent_status_ordering;
#[path = "agent_status_policy.rs"]
mod agent_status_policy;
#[path = "agent_status_second_report.rs"]
mod agent_status_second_report;
#[path = "attachment_carriers_loopback.rs"]
mod attachment_carriers_loopback;
#[path = "attachment_carriers_peer.rs"]
mod attachment_carriers_peer;
#[path = "attachment_packets.rs"]
mod attachment_packets;
#[path = "attachments.rs"]
mod attachments;
#[path = "attachments_fallback.rs"]
mod attachments_fallback;
#[path = "attachments_relay.rs"]
mod attachments_relay;
#[path = "attachments_transfer.rs"]
mod attachments_transfer;
#[path = "auth_ceremony.rs"]
mod auth_ceremony;
#[path = "auth_device_key.rs"]
mod auth_device_key;
#[path = "auth_first_boot_race.rs"]
mod auth_first_boot_race;
#[path = "backfill_history_ranges.rs"]
mod backfill_history_ranges;
#[path = "browse_machine_scope.rs"]
mod browse_machine_scope;
#[path = "browse_server_switch.rs"]
mod browse_server_switch;
#[path = "carrier_wire.rs"]
mod carrier_wire;
#[path = "carriers_grantless_open.rs"]
mod carriers_grantless_open;
#[path = "carriers_phase_timing.rs"]
mod carriers_phase_timing;
#[path = "carriers_prewarm.rs"]
mod carriers_prewarm;
#[path = "connect_interceptor.rs"]
mod connect_interceptor;
#[path = "core_without_a_browser.rs"]
mod core_without_a_browser;
#[path = "deck_intent.rs"]
mod deck_intent;
#[path = "deck_spawn.rs"]
mod deck_spawn;
#[path = "deck_tab_badge.rs"]
mod deck_tab_badge;
#[path = "deck_view.rs"]
mod deck_view;
#[path = "deck_warm_set.rs"]
mod deck_warm_set;
#[path = "direct_carrier_lane.rs"]
mod direct_carrier_lane;
#[path = "direct_carrier_promotion.rs"]
mod direct_carrier_promotion;
#[path = "direct_carrier_retirement.rs"]
mod direct_carrier_retirement;
#[path = "direct_carrier_route_loss.rs"]
mod direct_carrier_route_loss;
#[path = "direct_carrier_staging.rs"]
mod direct_carrier_staging;
#[path = "download_transfer.rs"]
mod download_transfer;
#[path = "folder_name_validation.rs"]
mod folder_name_validation;
#[path = "global_search_across_machines.rs"]
mod global_search_across_machines;
#[path = "global_search_fencing.rs"]
mod global_search_fencing;
#[path = "history_backfill.rs"]
mod history_backfill;
#[path = "input_outcome_feed.rs"]
mod input_outcome_feed;
#[path = "layout_apply_recovery.rs"]
mod layout_apply_recovery;
#[path = "layout_apply_target.rs"]
mod layout_apply_target;
#[path = "layout_document_apply.rs"]
mod layout_document_apply;
#[path = "layout_geometry_records.rs"]
mod layout_geometry_records;
#[path = "layout_pane_tree.rs"]
mod layout_pane_tree;
#[path = "layout_presets.rs"]
mod layout_presets;
#[path = "layout_two_clients.rs"]
mod layout_two_clients;
#[path = "local_discovery.rs"]
mod local_discovery;
#[path = "local_terminal_credential.rs"]
mod local_terminal_credential;
#[path = "local_terminal_grant_backoff.rs"]
mod local_terminal_grant_backoff;
#[path = "local_terminal_grant_fences.rs"]
mod local_terminal_grant_fences;
#[path = "local_terminal_grant_scope.rs"]
mod local_terminal_grant_scope;
#[path = "local_terminal_ready.rs"]
mod local_terminal_ready;
#[path = "local_terminal_route.rs"]
mod local_terminal_route;
#[path = "navigation_index.rs"]
mod navigation_index;
#[path = "navigation_query.rs"]
mod navigation_query;
#[path = "palette_catalog.rs"]
mod palette_catalog;
#[path = "predictive_echo_ack.rs"]
mod predictive_echo_ack;
#[path = "predictive_echo_confidence.rs"]
mod predictive_echo_confidence;
#[path = "predictive_echo_epoch.rs"]
mod predictive_echo_epoch;
#[path = "predictive_echo_gate.rs"]
mod predictive_echo_gate;
#[path = "predictive_echo_reset.rs"]
mod predictive_echo_reset;
#[path = "predictive_echo_seed.rs"]
mod predictive_echo_seed;
#[path = "prefs_persistence.rs"]
mod prefs_persistence;
#[path = "replica_frame_counts.rs"]
mod replica_frame_counts;
#[path = "rpc_codec_identity.rs"]
mod rpc_codec_identity;
#[path = "rpc_codec_requests.rs"]
mod rpc_codec_requests;
#[path = "rpc_codec_responses.rs"]
mod rpc_codec_responses;
#[path = "rpc_codec_rows.rs"]
mod rpc_codec_rows;
#[path = "shell_intent.rs"]
mod shell_intent;
#[path = "sidebar_intent.rs"]
mod sidebar_intent;
#[path = "smoke_input_observer.rs"]
mod smoke_input_observer;
#[path = "store_close_kill.rs"]
mod store_close_kill;
#[path = "store_revision.rs"]
mod store_revision;
#[path = "store_selectors.rs"]
mod store_selectors;
#[path = "store_sidebar.rs"]
mod store_sidebar;
#[path = "store_spawn.rs"]
mod store_spawn;
#[path = "store_toasts.rs"]
mod store_toasts;
#[path = "store_transfers.rs"]
mod store_transfers;
#[path = "sync_access_publication.rs"]
mod sync_access_publication;
#[path = "sync_close_codes.rs"]
mod sync_close_codes;
#[path = "sync_decode_controls.rs"]
mod sync_decode_controls;
#[path = "sync_decode_meta.rs"]
mod sync_decode_meta;
#[path = "sync_decode_registry.rs"]
mod sync_decode_registry;
#[path = "sync_decode_routable.rs"]
mod sync_decode_routable;
#[path = "sync_decode_sessions.rs"]
mod sync_decode_sessions;
#[path = "sync_domain_reset.rs"]
mod sync_domain_reset;
#[path = "sync_encode.rs"]
mod sync_encode;
#[path = "sync_encode_terminal.rs"]
mod sync_encode_terminal;
#[path = "sync_generation_fence.rs"]
mod sync_generation_fence;
#[path = "sync_reconnect_placement.rs"]
mod sync_reconnect_placement;
#[path = "sync_redial_pacing.rs"]
mod sync_redial_pacing;
#[path = "sync_rehydration_prunes_nothing.rs"]
mod sync_rehydration_prunes_nothing;
#[path = "tab_identity.rs"]
mod tab_identity;
#[path = "terminal_chunk_conformance.rs"]
mod terminal_chunk_conformance;
#[path = "terminal_epoch_fence.rs"]
mod terminal_epoch_fence;
#[path = "terminal_foreground_liveness.rs"]
mod terminal_foreground_liveness;
#[path = "terminal_full_before_delta.rs"]
mod terminal_full_before_delta;
#[path = "terminal_input_route_claim.rs"]
mod terminal_input_route_claim;
#[path = "terminal_input_route_loss.rs"]
mod terminal_input_route_loss;
#[path = "terminal_liveness_idle_backoff.rs"]
mod terminal_liveness_idle_backoff;
#[path = "terminal_liveness_retirement.rs"]
mod terminal_liveness_retirement;
#[path = "terminal_nav_pad.rs"]
mod terminal_nav_pad;
#[path = "terminal_pane_rpc.rs"]
mod terminal_pane_rpc;
#[path = "terminal_peer_admission.rs"]
mod terminal_peer_admission;
#[path = "terminal_peer_election.rs"]
mod terminal_peer_election;
#[path = "terminal_peer_fallback_grants.rs"]
mod terminal_peer_fallback_grants;
#[path = "terminal_peer_fallback_handover.rs"]
mod terminal_peer_fallback_handover;
#[path = "terminal_peer_grant_refresh.rs"]
mod terminal_peer_grant_refresh;
#[path = "terminal_peer_negotiation.rs"]
mod terminal_peer_negotiation;
#[path = "terminal_peer_route_timing.rs"]
mod terminal_peer_route_timing;
#[path = "terminal_peer_trait.rs"]
mod terminal_peer_trait;
#[path = "terminal_renderer_deliveries.rs"]
mod terminal_renderer_deliveries;
#[path = "terminal_repair_on_refusal.rs"]
mod terminal_repair_on_refusal;
#[path = "terminal_smoke_faults.rs"]
mod terminal_smoke_faults;
#[path = "terminal_transport_indicator.rs"]
mod terminal_transport_indicator;
#[path = "terminal_view_answer.rs"]
mod terminal_view_answer;
#[path = "terminal_view_state_stream.rs"]
mod terminal_view_state_stream;
#[path = "transport_probe.rs"]
mod transport_probe;
#[path = "ui_command_drain.rs"]
mod ui_command_drain;
#[path = "ui_command_layout_map.rs"]
mod ui_command_layout_map;
#[path = "ui_state_apply_result.rs"]
mod ui_state_apply_result;
#[path = "ui_state_report.rs"]
mod ui_state_report;
