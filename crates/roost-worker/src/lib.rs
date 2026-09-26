//! The worker service: sessions, the keeper client, the durable outbox, the coordinator link, the local door, the WebRTC peer, and agent tracking. Never depends on roost-coord.
//! Owned here until a phase fills it in; nothing calls it yet.

#![forbid(unsafe_code)]

pub mod agent_occupancy;
pub mod attachment_transfer;
pub mod backoff;
pub mod boot_keeper;
pub mod browser_commands;
pub mod channel_fsm;
pub mod diag_snapshot;
pub mod event_store;
pub mod link_barrier;
pub mod link_dial;
pub mod local_door;
pub mod outbox;
pub mod runtime;

// The crate-root contract a host depends on: `serve` blocks until the worker is
// asked to stop, and `WorkerBoot` is the already-resolved configuration it
// takes. Re-exported here so the CLI binds to a contract rather than to the
// shape of the module tree behind it.
pub use runtime::{WorkerBoot, WorkerOverrides, serve, serve_until};
pub mod scrollback_read;
pub mod strays;
pub mod stream_fence;
