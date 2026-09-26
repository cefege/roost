//! The worker registry, the coordinator→worker send path, and the five worker
//! RPCs. No socket: the WebSocket lifecycle, the handshake and the frame
//! dispatcher are a separate slice, and this one is what that slice calls.
//!
//! Ported from `apps/coord/src/workers/`. The map of live worker sockets is
//! `coord_core::worker_handle::WorkerRegistry` -- shared state, because a handle
//! is replaced on every hello -- so what lives here is the policy around it:
//! which generation may carry a frame, what becoming routable publishes, and
//! what a deletion tears down.
//!
//! THE FIVE METHODS AND THEIR HANDLERS are in `rpc::METHOD_HANDLERS`. Every
//! mutation leases from the one write gate, every projection of a row goes
//! through `projection`, and the terminal collaborators are reached only through
//! the two traits in `coord_core::seams` -- which is what lets every test in
//! `tests/workers_*.rs` drive a whole delete with no terminal hub in the process.

pub mod claims;
pub mod delete;
pub mod heartbeat;
pub mod live_effects;
pub mod projection;
pub mod register;
pub mod registry;
pub mod respawn;
pub mod rows;
pub mod rpc;
pub mod send;

pub use send::{SendOutcome, SendRefusal};
