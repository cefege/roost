//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "agent_conversation_restore.rs"]
mod agent_conversation_restore;
#[path = "agent_conversation_restore_reconcile.rs"]
mod agent_conversation_restore_reconcile;
#[path = "agent_manifest_rules.rs"]
mod agent_manifest_rules;
#[path = "agent_process_identity.rs"]
mod agent_process_identity;
#[path = "agent_prompt_control.rs"]
mod agent_prompt_control;
#[path = "agent_prompt_fences.rs"]
mod agent_prompt_fences;
#[path = "agent_prompt_foreground.rs"]
mod agent_prompt_foreground;
#[path = "agent_reference_admission.rs"]
mod agent_reference_admission;
#[path = "agent_report_environment.rs"]
mod agent_report_environment;
#[path = "agent_report_peer.rs"]
mod agent_report_peer;
#[path = "agent_report_server.rs"]
mod agent_report_server;
#[path = "agent_status_arbitration.rs"]
mod agent_status_arbitration;
#[path = "agent_status_exit_completion.rs"]
mod agent_status_exit_completion;
#[path = "agent_status_install_rollback.rs"]
mod agent_status_install_rollback;
#[path = "agent_status_installer.rs"]
mod agent_status_installer;
#[path = "agent_status_integration_ownership.rs"]
mod agent_status_integration_ownership;
#[path = "agent_status_link_e2e.rs"]
mod agent_status_link_e2e;
#[path = "agent_status_osc_transition.rs"]
mod agent_status_osc_transition;
#[path = "agent_status_peer_process_id.rs"]
mod agent_status_peer_process_id;
#[path = "agent_status_reference_clear.rs"]
mod agent_status_reference_clear;
#[path = "agent_status_registry_identity.rs"]
mod agent_status_registry_identity;
#[path = "agent_status_screen_gate.rs"]
mod agent_status_screen_gate;
#[path = "agent_status_stable_transitions.rs"]
mod agent_status_stable_transitions;
#[path = "attachment_direct_socket.rs"]
mod attachment_direct_socket;
#[path = "attachment_grants.rs"]
mod attachment_grants;
#[path = "attachment_loopback_upload.rs"]
mod attachment_loopback_upload;
#[path = "attachment_operation_owner.rs"]
mod attachment_operation_owner;
#[path = "attachment_operation_recovery.rs"]
mod attachment_operation_recovery;
#[path = "attachment_peer_owner.rs"]
mod attachment_peer_owner;
#[path = "attachment_peer_port.rs"]
mod attachment_peer_port;
#[path = "attachment_peer_upload.rs"]
mod attachment_peer_upload;
#[path = "attachment_reaper.rs"]
mod attachment_reaper;
#[path = "attachment_transfer_lease.rs"]
mod attachment_transfer_lease;
#[path = "attachment_upload.rs"]
mod attachment_upload;
#[path = "backoff_policy.rs"]
mod backoff_policy;
#[path = "boot_admission.rs"]
mod boot_admission;
#[path = "boot_adoption_gate.rs"]
mod boot_adoption_gate;
#[path = "boot_keeper.rs"]
mod boot_keeper;
#[path = "browser_command_attachments.rs"]
mod browser_command_attachments;
#[path = "browser_command_diagnostics.rs"]
mod browser_command_diagnostics;
#[path = "browser_command_files.rs"]
mod browser_command_files;
#[path = "browser_command_terminal.rs"]
mod browser_command_terminal;
#[path = "browser_commands.rs"]
mod browser_commands;
#[path = "capture_ack.rs"]
mod capture_ack;
#[path = "capture_assembly.rs"]
mod capture_assembly;
#[path = "capture_evidence.rs"]
mod capture_evidence;
#[path = "capture_real_session.rs"]
mod capture_real_session;
#[path = "capture_recorder.rs"]
mod capture_recorder;
#[path = "capture_replay.rs"]
mod capture_replay;
#[path = "capture_resize.rs"]
mod capture_resize;
#[path = "capture_storage.rs"]
mod capture_storage;
#[path = "capture_wire.rs"]
mod capture_wire;
#[path = "cell_cadence.rs"]
mod cell_cadence;
#[path = "cell_cadence_frame_rate.rs"]
mod cell_cadence_frame_rate;
#[path = "cell_frame_sustained_rate.rs"]
mod cell_frame_sustained_rate;
#[path = "cell_row_json.rs"]
mod cell_row_json;
#[path = "cell_scheduler.rs"]
mod cell_scheduler;
#[path = "cell_sinks.rs"]
mod cell_sinks;
#[path = "cell_sync_output.rs"]
mod cell_sync_output;
#[path = "cell_window_grid_rate.rs"]
mod cell_window_grid_rate;
#[path = "cgroup_throttle_health.rs"]
mod cgroup_throttle_health;
#[path = "channel_creation_gate.rs"]
mod channel_creation_gate;
#[path = "channel_fsm.rs"]
mod channel_fsm;
#[path = "control_lane_order.rs"]
mod control_lane_order;
#[path = "diag_sessions.rs"]
mod diag_sessions;
#[path = "diag_snapshot.rs"]
mod diag_snapshot;
#[path = "direct_terminal.rs"]
mod direct_terminal;
#[path = "durable_delivery.rs"]
mod durable_delivery;
#[path = "durable_outbox.rs"]
mod durable_outbox;
#[path = "durable_outbox_claims.rs"]
mod durable_outbox_claims;
#[path = "durable_outbox_coalescing.rs"]
mod durable_outbox_coalescing;
#[path = "enrollment_round_trip.rs"]
mod enrollment_round_trip;
#[path = "event_store.rs"]
mod event_store;
#[path = "global_search_page_budget.rs"]
mod global_search_page_budget;
#[path = "heartbeat.rs"]
mod heartbeat;
#[path = "heartbeat_host_metrics.rs"]
mod heartbeat_host_metrics;
#[path = "host_folder_facts.rs"]
mod host_folder_facts;
#[path = "host_identity_facts.rs"]
mod host_identity_facts;
#[path = "host_listening_ports.rs"]
mod host_listening_ports;
#[path = "host_pr_status.rs"]
mod host_pr_status;
#[path = "host_samples.rs"]
mod host_samples;
#[path = "keeper_maintenance_admission.rs"]
mod keeper_maintenance_admission;
#[path = "keeper_pool_channels.rs"]
mod keeper_pool_channels;
#[path = "keeper_pool_spawn.rs"]
mod keeper_pool_spawn;
#[path = "keeper_probe_digest.rs"]
mod keeper_probe_digest;
#[path = "keeper_survivor_adoption.rs"]
mod keeper_survivor_adoption;
#[path = "keeper_update_action.rs"]
mod keeper_update_action;
#[path = "keeper_update_prepare.rs"]
mod keeper_update_prepare;
#[path = "keeper_update_prepare_drain.rs"]
mod keeper_update_prepare_drain;
#[path = "keeper_update_preservation.rs"]
mod keeper_update_preservation;
#[path = "keeper_update_terminal_freeze.rs"]
mod keeper_update_terminal_freeze;
#[path = "launch_and_channel_vocabulary.rs"]
mod launch_and_channel_vocabulary;
#[path = "link_agent_status.rs"]
mod link_agent_status;
#[path = "link_barrier.rs"]
mod link_barrier;
#[path = "link_dial.rs"]
mod link_dial;
#[path = "link_downstream_absent.rs"]
mod link_downstream_absent;
#[path = "link_downstream_agent_prompt.rs"]
mod link_downstream_agent_prompt;
#[path = "link_downstream_attachment_peer.rs"]
mod link_downstream_attachment_peer;
#[path = "link_downstream_attachments.rs"]
mod link_downstream_attachments;
#[path = "link_downstream_direct.rs"]
mod link_downstream_direct;
#[path = "link_downstream_keeper_update.rs"]
mod link_downstream_keeper_update;
#[path = "link_downstream_live.rs"]
mod link_downstream_live;
#[path = "link_downstream_local_grant.rs"]
mod link_downstream_local_grant;
#[path = "link_downstream_stream.rs"]
mod link_downstream_stream;
#[path = "link_downstream_terminal.rs"]
mod link_downstream_terminal;
#[path = "link_downstream_uplink.rs"]
mod link_downstream_uplink;
#[path = "link_downstream_writable.rs"]
mod link_downstream_writable;
#[path = "link_repair_order.rs"]
mod link_repair_order;
#[path = "link_wire_parity.rs"]
mod link_wire_parity;
#[path = "list_dir_wire_contract.rs"]
mod list_dir_wire_contract;
#[path = "local_door_http.rs"]
mod local_door_http;
#[path = "local_door_sockets.rs"]
mod local_door_sockets;
#[path = "local_door_spa.rs"]
mod local_door_spa;
#[path = "local_door_terminal.rs"]
mod local_door_terminal;
#[path = "local_terminal_grants.rs"]
mod local_terminal_grants;
#[path = "local_terminal_peer_socket.rs"]
mod local_terminal_peer_socket;
#[path = "local_terminal_prehello.rs"]
mod local_terminal_prehello;
#[path = "local_terminal_pty.rs"]
mod local_terminal_pty;
#[path = "local_terminal_scrollback.rs"]
mod local_terminal_scrollback;
#[path = "local_terminal_socket.rs"]
mod local_terminal_socket;
#[path = "local_terminal_socket_input.rs"]
mod local_terminal_socket_input;
#[path = "open_session_credential.rs"]
mod open_session_credential;
#[path = "open_session_set.rs"]
mod open_session_set;
#[path = "outbox_order.rs"]
mod outbox_order;
#[path = "query_reply.rs"]
mod query_reply;
#[path = "query_reply_lane.rs"]
mod query_reply_lane;
#[path = "query_reply_replay_align.rs"]
mod query_reply_replay_align;
#[path = "query_reply_unhandled.rs"]
mod query_reply_unhandled;
#[path = "reconcile_gate.rs"]
mod reconcile_gate;
#[path = "retained_grid.rs"]
mod retained_grid;
#[path = "scrollback_page_contiguity.rs"]
mod scrollback_page_contiguity;
#[path = "scrollback_page_window.rs"]
mod scrollback_page_window;
#[path = "scrollback_policy.rs"]
mod scrollback_policy;
#[path = "scrollback_read.rs"]
mod scrollback_read;
#[path = "session_adoption.rs"]
mod session_adoption;
#[path = "session_binding.rs"]
mod session_binding;
#[path = "session_cell_emit.rs"]
mod session_cell_emit;
#[path = "session_cell_sink.rs"]
mod session_cell_sink;
#[path = "session_cwd_event.rs"]
mod session_cwd_event;
#[path = "session_git_ports.rs"]
mod session_git_ports;
#[path = "session_ids.rs"]
mod session_ids;
#[path = "session_lifecycle.rs"]
mod session_lifecycle;
#[path = "session_raw_metadata.rs"]
mod session_raw_metadata;
#[path = "session_resize.rs"]
mod session_resize;
#[path = "session_resume_teardown.rs"]
mod session_resume_teardown;
#[path = "session_spawn.rs"]
mod session_spawn;
#[path = "session_vocabulary.rs"]
mod session_vocabulary;
#[path = "shell_spec_resolution.rs"]
mod shell_spec_resolution;
#[path = "stray_reap.rs"]
mod stray_reap;
#[path = "strays.rs"]
mod strays;
#[path = "stream_fence.rs"]
mod stream_fence;
#[path = "stream_scan_bytes.rs"]
mod stream_scan_bytes;
#[path = "terminal_core_capacity.rs"]
mod terminal_core_capacity;
#[path = "terminal_core_capacity_session.rs"]
mod terminal_core_capacity_session;
#[path = "terminal_input_e2e.rs"]
mod terminal_input_e2e;
#[path = "terminal_input_order.rs"]
mod terminal_input_order;
#[path = "terminal_input_route_owner.rs"]
mod terminal_input_route_owner;
#[path = "terminal_input_work_budget.rs"]
mod terminal_input_work_budget;
#[path = "terminal_input_write.rs"]
mod terminal_input_write;
#[path = "terminal_metadata.rs"]
mod terminal_metadata;
#[path = "terminal_peer_offer_faults.rs"]
mod terminal_peer_offer_faults;
#[path = "terminal_peer_owner.rs"]
mod terminal_peer_owner;
#[path = "terminal_peer_packet_faults.rs"]
mod terminal_peer_packet_faults;
#[path = "terminal_peer_packet_ingress.rs"]
mod terminal_peer_packet_ingress;
#[path = "terminal_peer_packet_port.rs"]
mod terminal_peer_packet_port;
#[path = "terminal_peer_str0m.rs"]
mod terminal_peer_str0m;
#[path = "terminal_pipeline.rs"]
mod terminal_pipeline;
#[path = "terminal_pipeline_bounds.rs"]
mod terminal_pipeline_bounds;
#[path = "terminal_stream_core_trap.rs"]
mod terminal_stream_core_trap;
#[path = "terminal_stream_delivery.rs"]
mod terminal_stream_delivery;
#[path = "terminal_stream_keeper.rs"]
mod terminal_stream_keeper;
#[path = "terminal_stream_state.rs"]
mod terminal_stream_state;
#[path = "terminal_view_owner.rs"]
mod terminal_view_owner;
#[path = "terminal_view_relay.rs"]
mod terminal_view_relay;
#[path = "worker_boot_config.rs"]
mod worker_boot_config;
#[path = "worker_boot_identity.rs"]
mod worker_boot_identity;
#[path = "worker_boot_order.rs"]
mod worker_boot_order;
#[path = "worker_credential.rs"]
mod worker_credential;
#[path = "worker_credential_identity.rs"]
mod worker_credential_identity;
#[path = "worker_keeper_admission.rs"]
mod worker_keeper_admission;
#[path = "worker_reconnect_ladder.rs"]
mod worker_reconnect_ladder;
#[path = "worker_retire_authorization.rs"]
mod worker_retire_authorization;
#[path = "worker_service_definition_scrub.rs"]
mod worker_service_definition_scrub;
#[path = "worker_shutdown_boundary.rs"]
mod worker_shutdown_boundary;
