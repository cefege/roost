//! The worker WebSocket: upgrade admission, the socket loop and its post-hello
//! session, the frame classifier and dispatcher, the per-socket durable-event
//! window, and the announced-channel barrier.
//!
//! Owned by the coordinator. `connection`, `handshake` and `result_lane` are the
//! only modules that read a socket, and `link_session` the only one holding a link's state;
//! admission, the heartbeat, the rate window, the classifier and the barrier
//! are pure over values and a clock they are handed, because the ORDER of those
//! decisions is the property worth testing.
//!
//! The frame vocabulary and the limits both come from
//! `protocol/spec/worker-link.md`, and each constant here cites the incident or
//! the arithmetic that fixes its value.
//!
//! v2's type-only `workers/worker-conn-types.ts` has no file here because it
//! had no behaviour to port: `WorkerConn` is this module's `connection`, its
//! `WorkerServiceDeps` bag is `CoordServices` (`services.rs`), its
//! `WorkerUpdateProgress` is a generated `roost_protocol::wire::coord_worker`
//! message, and its `TerminalInputRouteResultSink` is
//! `terminal_input::route_results`.

pub mod announced_barrier;
mod announced_channel;
mod announced_lane;
pub mod announced_retention;
pub mod announced_types;
pub mod client_seq;
pub mod conn_types;
pub mod connection;
pub mod direct_results;
pub mod dispatch;
pub mod dispatcher_for;
mod downstream_write;
pub mod frame_dispatch;
pub mod frame_queue;
pub mod handshake;
pub mod keepalive;
mod link_session;
mod link_upkeep;
pub mod live_frames;
pub mod rate_window;
mod reap_outbox;
mod result_lane;
pub mod retained_budget;
pub mod upgrade_admission;
pub mod upstream_frame;
