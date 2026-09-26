//! The Sync WebSocket: upgrade admission and the cumulative delivery-sequence ACK
//! window.
//!
//! Owned by the coordinator. Both modules are pure over an injected clock and an
//! injected "did the socket accept it" answer, so every bound and every close path
//! is testable without a socket.
//!
//! The limits are fixed by `protocol/spec/sync.md` §Limits and each is cited at
//! its definition here; the close codes are `1013` for backpressure and `1008`
//! for an acknowledgement above the last sent sequence.

pub mod ack_window;
pub mod upgrade_admission;
