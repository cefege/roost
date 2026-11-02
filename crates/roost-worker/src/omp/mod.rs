//! Worker-owned ACP backend for OMP chat conversations.
//! The runtime routes prompt/cancel/ACK/close commands to a per-chat actor.
//! Process credentials remain machine-local; only projected events leave it.

mod actor;
mod emitter;
pub mod launcher;
mod owner;
mod session;

pub use owner::{OmpLinkLifecycle, OmpOwner, OmpResend};
