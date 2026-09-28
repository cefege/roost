//! The worker WebSocket: upgrade admission, the socket loop and its post-hello
//! session, the frame classifier and dispatcher, the per-socket durable-event
//! window, and the announced-channel barrier.
//!
//! Owned by the coordinator. `connection` and `handshake` are the only modules
//! that read a socket, and `link_session` the only one holding a link's state;
//! admission, the heartbeat, the rate window, the classifier and the barrier
//! are pure over values and a clock they are handed, because the ORDER of those
//! decisions is the property worth testing.
//!
//! The frame vocabulary and the limits both come from
//! `protocol/spec/worker-link.md`, and each constant here cites the incident or
//! the arithmetic that fixes its value.

pub mod announced_barrier;
pub mod announced_types;
pub mod client_seq;
pub mod conn_types;
pub mod connection;
pub mod dispatch;
pub mod dispatcher_for;
pub mod frame_dispatch;
pub mod frame_queue;
pub mod handshake;
pub mod keepalive;
mod link_session;
pub mod live_frames;
pub mod rate_window;
mod reap_outbox;
pub mod upgrade_admission;
pub mod upstream_frame;
