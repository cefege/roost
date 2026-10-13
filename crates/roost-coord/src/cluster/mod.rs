//! The shared-Postgres control plane for coordinator replicas.
//!
//! Link ownership elects the replica that serves worker and browser sockets;
//! durable notices wake replicas to re-read shared state.

pub mod heartbeat;
pub mod listener;
pub mod notify;
pub mod ownership;
pub mod runtime;
pub mod splice;
