//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "channel_history.rs"]
mod channel_history;
#[path = "codec_wire.rs"]
mod codec_wire;
#[path = "codec_wire_bounds.rs"]
mod codec_wire_bounds;
#[path = "history_wire.rs"]
mod history_wire;
#[path = "keeper_capability.rs"]
mod keeper_capability;
#[path = "keeper_child_reap.rs"]
mod keeper_child_reap;
#[path = "keeper_child_reap_sweep.rs"]
mod keeper_child_reap_sweep;
#[path = "keeper_client.rs"]
mod keeper_client;
#[path = "keeper_client_protocol.rs"]
mod keeper_client_protocol;
#[path = "keeper_contract.rs"]
mod keeper_contract;
#[path = "keeper_daemon.rs"]
mod keeper_daemon;
#[path = "keeper_daemon_cli.rs"]
mod keeper_daemon_cli;
#[path = "keeper_daemon_crash.rs"]
mod keeper_daemon_crash;
#[path = "keeper_daemon_exit.rs"]
mod keeper_daemon_exit;
#[path = "keeper_daemon_files.rs"]
mod keeper_daemon_files;
#[path = "keeper_dispatch.rs"]
mod keeper_dispatch;
#[path = "keeper_dispatch_input.rs"]
mod keeper_dispatch_input;
#[path = "keeper_endpoint.rs"]
mod keeper_endpoint;
#[path = "keeper_input_fidelity.rs"]
mod keeper_input_fidelity;
#[path = "keeper_input_queue.rs"]
mod keeper_input_queue;
#[path = "keeper_lifecycle.rs"]
mod keeper_lifecycle;
#[path = "keeper_queries.rs"]
mod keeper_queries;
#[path = "keeper_resize_outcome.rs"]
mod keeper_resize_outcome;
#[path = "keeper_socket.rs"]
mod keeper_socket;
#[path = "keeper_socket_auth.rs"]
mod keeper_socket_auth;
#[path = "keeper_socket_protocol.rs"]
mod keeper_socket_protocol;
#[path = "keeper_update_proof.rs"]
mod keeper_update_proof;
#[path = "output_echo_latency.rs"]
mod output_echo_latency;
#[path = "output_survives_control_round_trip.rs"]
mod output_survives_control_round_trip;
#[path = "output_tick_cadence.rs"]
mod output_tick_cadence;
#[path = "pty_channel.rs"]
mod pty_channel;
