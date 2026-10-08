//! The keeper daemon: PTY ownership, per-channel byte rings, and the framed
//! keeper socket protocol specified in protocol/spec/keeper.md. Shipped as a
//! separate binary so a coordinator deploy never disturbs a live PTY.
//! This is the one crate outside third_party/ permitted to use `unsafe`: it
//! owns raw file descriptors, the controlling-TTY handshake, and the Win32
//! console, Job Object and named-pipe calls (`win32_ffi` is the safe wrapper
//! other crates use); every call site must name the invariant it protects.

pub mod capability;
pub mod channel_history;
#[cfg(windows)]
mod channel_job;
pub mod client;
pub mod client_arrival;
pub mod client_connect;
pub mod client_error;
pub mod client_frames;
pub mod client_history;
pub mod client_io;
pub mod client_queries;
pub mod client_resize;
pub mod codec;
pub mod frames;
pub mod history;
pub mod input_queue;
pub mod keeper;
pub mod keeper_ops;
mod keeper_reap;
pub mod output_ring;
pub mod owner_only;
pub mod payloads;
mod process_epoch;
pub mod process_reap;
pub mod pty_channel;
mod pty_channel_reap;
pub mod server;
pub mod transport;
#[cfg(windows)]
pub mod win32_ffi;
