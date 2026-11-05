//! Worker-owned ACP backend for OMP chat conversations.
//! The runtime routes prompt/cancel/ACK/close commands to a per-chat actor.
//! Process credentials remain machine-local; only projected events leave it.

mod actor;
mod actor_enqueue;
mod actor_event;
mod actor_types;
mod connection_loop;
mod emitter;
mod interaction_pending;
pub mod launcher;
mod open_request;
mod owner;
mod session;
mod ui;

pub use owner::{OmpLinkLifecycle, OmpOwner, OmpResend};
