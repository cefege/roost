//! The worker service: sessions, the keeper client, the durable outbox, the coordinator link, the local door, the WebRTC peer, and agent tracking. Never depends on roost-coord.
//! Each module below is a DECLARATION OF OWNERSHIP: `session`, `door`, `peer`,
//! `agents`, `attachments`, `capture`, `host` and `keeper_pool` name the
//! concepts the worker's later waves fill, and each already holds the shared
//! types its slices compile against. Nothing calls most of them yet, and a
//! module with no caller is a module whose owner has not started.

#![forbid(unsafe_code)]

pub mod agent_occupancy;
pub mod agents;
pub mod attachment_transfer;
pub mod attachments;
pub mod backoff;
pub mod boot_keeper;
pub mod browser_commands;
pub mod capture;
pub mod channel_fsm;
pub mod diag_snapshot;
pub mod door;
pub mod event_store;
pub mod host;
pub mod keeper_pool;
pub mod link_barrier;
pub mod link_dial;
pub mod local_door;
pub mod outbox;
pub mod peer;
pub mod runtime;

// The crate-root contract a host depends on: `serve` blocks until the worker is
// asked to stop, and `WorkerBoot` is the already-resolved configuration it
// takes. Re-exported here so the CLI binds to a contract rather than to the
// shape of the module tree behind it.
pub use runtime::{WorkerBoot, WorkerOverrides, serve, serve_until};
pub mod scrollback_read;
pub mod session;
pub mod shell_spec;
pub mod strays;
pub mod stream_fence;
