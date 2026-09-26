//! The worker WebSocket: upgrade admission, the per-socket durable-event window,
//! and the announced-channel barrier that holds a new channel's first frames
//! until its durable route commits.
//!
//! Owned by the coordinator. Every module here is pure -- no socket, no database,
//! no clock of its own -- because the admission ORDER and the barrier's two
//! phases are the properties worth testing and neither is observable through a
//! live socket without flakiness.
//!
//! The frame vocabulary and the limits both come from
//! `protocol/spec/worker-link.md`, and each constant here cites the incident or
//! the arithmetic that fixes its value.

pub mod announced_barrier;
pub mod announced_types;
pub mod rate_window;
pub mod upgrade_admission;
