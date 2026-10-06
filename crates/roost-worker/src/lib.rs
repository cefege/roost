//! The worker service crate root: sessions, the keeper client, the durable outbox,
//! the coordinator link, the local door, the WebRTC peer, and agent tracking.
//! Called by `roost-cli` through `serve`/`WorkerBoot`; never depends on roost-coord.

#![forbid(unsafe_code)]

pub mod agent_occupancy;
pub mod agents;
pub mod attachments;
pub mod backoff;
pub mod boot_keeper;
pub mod browser_commands;
pub mod capture;
pub mod channel_fsm;
pub mod coordinator_tls;
pub mod diag_snapshot;
pub mod door;
pub mod event_store;
pub mod host;
pub mod keeper_pool;
pub mod link_barrier;
pub mod link_dial;
pub mod local_door;
pub mod local_terminal;
pub mod outbox;
pub mod peer;
pub mod runtime;

// The crate-root contract a host depends on: `serve` blocks until the worker is
// asked to stop, and `WorkerBoot` is the already-resolved configuration it
// takes. Re-exported here so the CLI binds to a contract rather than to the
// shape of the module tree behind it.
pub use runtime::{WorkerBoot, WorkerOverrides, serve, serve_until};
pub mod link_ports;
pub mod scrollback_read;
pub mod session;
pub mod shell_spec;
pub mod strays;
pub mod stream_fence;
pub mod terminal_core_capacity;
pub mod terminal_input;
pub mod terminal_pipeline;
pub mod terminal_view;
pub mod uplink;
