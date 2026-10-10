//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "agent_chat_rpc.rs"]
mod agent_chat_rpc;
#[path = "agent_config_rpc.rs"]
mod agent_config_rpc;
#[path = "agent_prompt_handlers.rs"]
mod agent_prompt_handlers;
#[path = "agent_prompt_status_wait.rs"]
mod agent_prompt_status_wait;
#[path = "agent_status_ordering.rs"]
mod agent_status_ordering;
#[path = "agent_status_push.rs"]
mod agent_status_push;
#[path = "agent_status_retirement.rs"]
mod agent_status_retirement;
#[path = "agent_status_rpc.rs"]
mod agent_status_rpc;
#[path = "agent_status_wait.rs"]
mod agent_status_wait;
#[path = "agent_store.rs"]
mod agent_store;
#[path = "agent_tool_calls.rs"]
mod agent_tool_calls;
#[path = "announced_barrier.rs"]
mod announced_barrier;
#[path = "announced_retention.rs"]
mod announced_retention;
#[path = "announced_socket.rs"]
mod announced_socket;
#[path = "attachments_direct_handlers.rs"]
mod attachments_direct_handlers;
#[path = "attachments_direct_status_results.rs"]
mod attachments_direct_status_results;
#[path = "attachments_file_rpcs.rs"]
mod attachments_file_rpcs;
#[path = "attachments_grant_owner.rs"]
mod attachments_grant_owner;
#[path = "attachments_peer_negotiations.rs"]
mod attachments_peer_negotiations;
#[path = "attachments_worker_link.rs"]
mod attachments_worker_link;
#[path = "boot_authorized_keys.rs"]
mod boot_authorized_keys;
#[path = "boot_database.rs"]
mod boot_database;
#[path = "boot_facts.rs"]
mod boot_facts;
#[path = "bootstrap_single_use.rs"]
mod bootstrap_single_use;
#[path = "cf_access_identity.rs"]
mod cf_access_identity;
#[path = "cf_access_keyring.rs"]
mod cf_access_keyring;
#[path = "client_seq_cursor.rs"]
mod client_seq_cursor;
#[path = "clipboard_history.rs"]
mod clipboard_history;
#[path = "db_export_snapshots.rs"]
mod db_export_snapshots;
#[path = "deploy_catchup.rs"]
mod deploy_catchup;
#[path = "deploy_catchup_decision.rs"]
mod deploy_catchup_decision;
#[path = "deploy_jobs.rs"]
mod deploy_jobs;
#[path = "deploy_output_http.rs"]
mod deploy_output_http;
#[path = "deploy_rpc.rs"]
mod deploy_rpc;
#[path = "deploy_update_progress.rs"]
mod deploy_update_progress;
#[path = "device_refusal_through_gate.rs"]
mod device_refusal_through_gate;
#[path = "diag_snapshot_fanout.rs"]
mod diag_snapshot_fanout;
#[path = "diag_snapshot_filters.rs"]
mod diag_snapshot_filters;
#[path = "diag_snapshot_probe_join.rs"]
mod diag_snapshot_probe_join;
#[path = "diag_snapshot_session_state.rs"]
mod diag_snapshot_session_state;
#[path = "diagnostics_rpc.rs"]
mod diagnostics_rpc;
#[path = "event_admission.rs"]
mod event_admission;
#[path = "event_append.rs"]
mod event_append;
#[path = "event_bus.rs"]
mod event_bus;
#[path = "event_publication.rs"]
mod event_publication;
#[path = "event_query.rs"]
mod event_query;
#[path = "event_snapshot_cap.rs"]
mod event_snapshot_cap;
#[path = "jwt_parity.rs"]
mod jwt_parity;
#[path = "keeper_update_admission.rs"]
mod keeper_update_admission;
#[path = "keeper_update_drain.rs"]
mod keeper_update_drain;
#[path = "keeper_update_refusals.rs"]
mod keeper_update_refusals;
#[path = "maintenance_audit_allowlist.rs"]
mod maintenance_audit_allowlist;
#[path = "maintenance_audit_batching.rs"]
mod maintenance_audit_batching;
#[path = "maintenance_audit_static_backlog.rs"]
mod maintenance_audit_static_backlog;
#[path = "maintenance_audit_window.rs"]
mod maintenance_audit_window;
#[path = "maintenance_backup.rs"]
mod maintenance_backup;
#[path = "maintenance_startup_janitor.rs"]
mod maintenance_startup_janitor;
#[path = "mcp_relays_authority.rs"]
mod mcp_relays_authority;
#[path = "mcp_relays_refusals.rs"]
mod mcp_relays_refusals;
#[path = "mcp_relays_registry.rs"]
mod mcp_relays_registry;
#[path = "mcp_relays_tenancy.rs"]
mod mcp_relays_tenancy;
#[path = "method_route_arm_pairing.rs"]
mod method_route_arm_pairing;
#[path = "method_route_coverage.rs"]
mod method_route_coverage;
#[path = "method_route_implementation.rs"]
mod method_route_implementation;
#[path = "middleware_admission_stack.rs"]
mod middleware_admission_stack;
#[path = "middleware_audit.rs"]
mod middleware_audit;
#[path = "middleware_caller_origin.rs"]
mod middleware_caller_origin;
#[path = "middleware_rate_limit.rs"]
mod middleware_rate_limit;
#[path = "middleware_security_headers.rs"]
mod middleware_security_headers;
#[path = "middleware_spa.rs"]
mod middleware_spa;
#[path = "migration_history.rs"]
mod migration_history;
#[path = "orphan_kills.rs"]
mod orphan_kills;
#[path = "pairing_approval_machine.rs"]
mod pairing_approval_machine;
#[path = "pairing_approver_gate.rs"]
mod pairing_approver_gate;
#[path = "pairing_confirmation.rs"]
mod pairing_confirmation;
#[path = "pairing_confirmation_authority.rs"]
mod pairing_confirmation_authority;
#[path = "pairing_provenance.rs"]
mod pairing_provenance;
#[path = "pairing_retention.rs"]
mod pairing_retention;
#[path = "pairing_retention_lifetime.rs"]
mod pairing_retention_lifetime;
#[path = "pairing_secrets.rs"]
mod pairing_secrets;
#[path = "public_redeem_through_gate.rs"]
mod public_redeem_through_gate;
#[path = "push_dispatch_fences.rs"]
mod push_dispatch_fences;
#[path = "push_dispatch_payload.rs"]
mod push_dispatch_payload;
#[path = "push_dispatch_targets.rs"]
mod push_dispatch_targets;
#[path = "push_pair_request.rs"]
mod push_pair_request;
#[path = "push_sender_bounds.rs"]
mod push_sender_bounds;
#[path = "push_sender_delivery.rs"]
mod push_sender_delivery;
#[path = "push_subscription_cap.rs"]
mod push_subscription_cap;
#[path = "push_subscription_rpc.rs"]
mod push_subscription_rpc;
#[path = "push_terminal_viewers.rs"]
mod push_terminal_viewers;
#[path = "push_transition_delivery.rs"]
mod push_transition_delivery;
#[path = "push_vapid_scope.rs"]
mod push_vapid_scope;
#[path = "push_web_push_transport.rs"]
mod push_web_push_transport;
#[path = "rate_limit_budget.rs"]
mod rate_limit_budget;
#[path = "rate_limit_refusals.rs"]
mod rate_limit_refusals;
#[path = "search_batch_validation.rs"]
mod search_batch_validation;
#[path = "search_control.rs"]
mod search_control;
#[path = "search_cursors.rs"]
mod search_cursors;
#[path = "search_fanout.rs"]
mod search_fanout;
#[path = "search_fanout_cursor.rs"]
mod search_fanout_cursor;
#[path = "search_progress.rs"]
mod search_progress;
#[path = "search_worker_lanes.rs"]
mod search_worker_lanes;
#[path = "self_hosted_tenant.rs"]
mod self_hosted_tenant;
#[path = "service_wiring.rs"]
mod service_wiring;
#[path = "service_wiring_ui_push.rs"]
mod service_wiring_ui_push;
#[path = "sessions_control_rpc.rs"]
mod sessions_control_rpc;
#[path = "sessions_list_auth.rs"]
mod sessions_list_auth;
#[path = "sessions_pending_rpc_drop.rs"]
mod sessions_pending_rpc_drop;
#[path = "sessions_pending_spawns.rs"]
mod sessions_pending_spawns;
#[path = "sessions_spawn_opened.rs"]
mod sessions_spawn_opened;
#[path = "sessions_spawn_rpc.rs"]
mod sessions_spawn_rpc;
#[path = "sync_client_frame_canonical.rs"]
mod sync_client_frame_canonical;
#[path = "sync_feed_adapters.rs"]
mod sync_feed_adapters;
#[path = "sync_feed_bus_coverage.rs"]
mod sync_feed_bus_coverage;
#[path = "sync_feed_volatile.rs"]
mod sync_feed_volatile;
#[path = "sync_layout_settlement.rs"]
mod sync_layout_settlement;
#[path = "sync_seed_paced.rs"]
mod sync_seed_paced;
#[path = "sync_seed_replay.rs"]
mod sync_seed_replay;
#[path = "sync_seed_scope.rs"]
mod sync_seed_scope;
#[path = "sync_seed_socket.rs"]
mod sync_seed_socket;
#[path = "sync_upgrade_admission.rs"]
mod sync_upgrade_admission;
#[path = "sync_v2_commands.rs"]
mod sync_v2_commands;
#[path = "sync_v2_send_queue.rs"]
mod sync_v2_send_queue;
#[path = "sync_v2_session.rs"]
mod sync_v2_session;
#[path = "sync_v2_terminal_lane.rs"]
mod sync_v2_terminal_lane;
#[path = "sync_ws_socket.rs"]
mod sync_ws_socket;
#[path = "sync_ws_socket_fences.rs"]
mod sync_ws_socket_fences;
#[path = "sync_ws_socket_lifecycle.rs"]
mod sync_ws_socket_lifecycle;
#[path = "sync_ws_socket_terminal.rs"]
mod sync_ws_socket_terminal;
#[path = "tasks_queue.rs"]
mod tasks_queue;
#[path = "tasks_refusals.rs"]
mod tasks_refusals;
#[path = "terminal_capture_admission.rs"]
mod terminal_capture_admission;
#[path = "terminal_capture_freeze.rs"]
mod terminal_capture_freeze;
#[path = "terminal_capture_lease.rs"]
mod terminal_capture_lease;
#[path = "terminal_capture_recorder.rs"]
mod terminal_capture_recorder;
#[path = "terminal_direct_grant_rpc.rs"]
mod terminal_direct_grant_rpc;
#[path = "terminal_direct_grants.rs"]
mod terminal_direct_grants;
#[path = "terminal_direct_link.rs"]
mod terminal_direct_link;
#[path = "terminal_direct_peer.rs"]
mod terminal_direct_peer;
#[path = "terminal_direct_peer_bounds.rs"]
mod terminal_direct_peer_bounds;
#[path = "terminal_hop_deadline.rs"]
mod terminal_hop_deadline;
#[path = "terminal_input_audit.rs"]
mod terminal_input_audit;
#[path = "terminal_input_control.rs"]
mod terminal_input_control;
#[path = "terminal_input_route_cache.rs"]
mod terminal_input_route_cache;
#[path = "terminal_input_route_fences.rs"]
mod terminal_input_route_fences;
#[path = "terminal_input_route_results.rs"]
mod terminal_input_route_results;
#[path = "terminal_input_sync.rs"]
mod terminal_input_sync;
#[path = "terminal_live_effects.rs"]
mod terminal_live_effects;
#[path = "terminal_screen_byte_hub.rs"]
mod terminal_screen_byte_hub;
#[path = "terminal_screen_fanout.rs"]
mod terminal_screen_fanout;
#[path = "terminal_screen_hub.rs"]
mod terminal_screen_hub;
#[path = "terminal_screen_hub_chunks.rs"]
mod terminal_screen_hub_chunks;
#[path = "terminal_screen_hub_hold.rs"]
mod terminal_screen_hub_hold;
#[path = "terminal_screen_hub_lifecycle.rs"]
mod terminal_screen_hub_lifecycle;
#[path = "terminal_screen_hub_snapshot.rs"]
mod terminal_screen_hub_snapshot;
#[path = "terminal_screen_image_rpc.rs"]
mod terminal_screen_image_rpc;
#[path = "terminal_screen_images.rs"]
mod terminal_screen_images;
#[path = "terminal_screen_pipeline_cache.rs"]
mod terminal_screen_pipeline_cache;
#[path = "terminal_screen_pipeline_snapshot.rs"]
mod terminal_screen_pipeline_snapshot;
#[path = "terminal_screen_pipeline_wire_shape.rs"]
mod terminal_screen_pipeline_wire_shape;
#[path = "terminal_screen_residency.rs"]
mod terminal_screen_residency;
#[path = "terminal_screen_residency_pool.rs"]
mod terminal_screen_residency_pool;
#[path = "terminal_screen_route_index.rs"]
mod terminal_screen_route_index;
#[path = "terminal_screen_scrollback.rs"]
mod terminal_screen_scrollback;
#[path = "terminal_screen_scrollback_pages.rs"]
mod terminal_screen_scrollback_pages;
#[path = "terminal_screen_scrollback_refusals.rs"]
mod terminal_screen_scrollback_refusals;
#[path = "terminal_screen_scrollback_search.rs"]
mod terminal_screen_scrollback_search;
#[path = "terminal_screen_search_result.rs"]
mod terminal_screen_search_result;
#[path = "terminal_screen_title.rs"]
mod terminal_screen_title;
#[path = "terminal_seam_wiring.rs"]
mod terminal_seam_wiring;
#[path = "terminal_signal_hub.rs"]
mod terminal_signal_hub;
#[path = "terminal_view_geometry.rs"]
mod terminal_view_geometry;
#[path = "terminal_view_membership.rs"]
mod terminal_view_membership;
#[path = "terminal_view_owner_screen.rs"]
mod terminal_view_owner_screen;
#[path = "terminal_view_relay.rs"]
mod terminal_view_relay;
#[path = "transcription_config.rs"]
mod transcription_config;
#[path = "transcription_probe.rs"]
mod transcription_probe;
#[path = "transport_windows_ack.rs"]
mod transport_windows_ack;
#[path = "ui_state_apply.rs"]
mod ui_state_apply;
#[path = "ui_state_apply_generations.rs"]
mod ui_state_apply_generations;
#[path = "ui_state_handlers.rs"]
mod ui_state_handlers;
#[path = "ui_state_layout_apply.rs"]
mod ui_state_layout_apply;
#[path = "ui_state_legacy_command.rs"]
mod ui_state_legacy_command;
#[path = "ui_state_reports.rs"]
mod ui_state_reports;
#[path = "upgrade_admission.rs"]
mod upgrade_admission;
#[path = "worker_frame_dispatch.rs"]
mod worker_frame_dispatch;
#[path = "worker_frame_ordering.rs"]
mod worker_frame_ordering;
#[path = "worker_link_core.rs"]
mod worker_link_core;
#[path = "worker_link_heartbeat.rs"]
mod worker_link_heartbeat;
#[path = "worker_link_result_lane.rs"]
mod worker_link_result_lane;
#[path = "worker_link_wire.rs"]
mod worker_link_wire;
#[path = "worker_live_frames.rs"]
mod worker_live_frames;
#[path = "worker_reap_delivery.rs"]
mod worker_reap_delivery;
#[path = "workers_handlers.rs"]
mod workers_handlers;
#[path = "workers_refusals.rs"]
mod workers_refusals;
#[path = "workers_registry.rs"]
mod workers_registry;
#[path = "workers_send.rs"]
mod workers_send;
#[path = "workers_send_result_dispatch.rs"]
mod workers_send_result_dispatch;
#[path = "workers_send_snapshot.rs"]
mod workers_send_snapshot;
#[path = "workers_send_terminal.rs"]
mod workers_send_terminal;
#[path = "workspaces_sync_delta.rs"]
mod workspaces_sync_delta;
#[path = "workspaces_tree.rs"]
mod workspaces_tree;
#[path = "workspaces_writes.rs"]
mod workspaces_writes;
#[path = "write_gate_and_principal.rs"]
mod write_gate_and_principal;
#[path = "ws_auth_deadline.rs"]
mod ws_auth_deadline;
