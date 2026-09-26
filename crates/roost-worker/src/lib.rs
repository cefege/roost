//! The worker service: sessions, the keeper client, the durable outbox, the coordinator link, the local door, the WebRTC peer, and agent tracking. Never depends on roost-coord.
//! Owned here until a phase fills it in; nothing calls it yet.

#![forbid(unsafe_code)]

pub mod backoff;
pub mod channel_fsm;
pub mod event_store;
pub mod link_barrier;
pub mod link_dial;
pub mod outbox;
pub mod strays;
