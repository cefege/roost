//! The Sync WebSocket's per-connection v2 session: upgrade admission, the
//! cumulative delivery-sequence ACK window, the bounded frame queues, the
//! weighted-lane scheduler, the terminal fan-out, and the client command path.
//!
//! Owned by the coordinator. Every module here is pure -- no socket, no clock of
//! its own, no database -- because the admission ORDER, the queue cutovers and
//! the terminal fences are the properties worth testing and none of them is
//! observable through a live socket without flakiness. The I/O shell that owns
//! the socket, encodes and writes frames, and mints socket ids is
//! `http::listener`'s caller, not this directory.
//!
//! THE SHARED STATE IS ONE TYPE, AND THAT IS THE POINT. A frame is charged to a
//! socket's retention budget by exactly one of {a domain queue, a terminal
//! cursor's materialisation, a cursor's delta tail, a lane's pending states},
//! and every release passes through one counter. v2 kept that record in seven
//! files whose closures all reached back into one `ws.data`; the port keeps the
//! record and splits the METHODS by concept -- queue ordering in `send_queue`,
//! admission and the flush turn in `egress`, terminal fan-out in `terminal/`,
//! the command path in `commands` -- because the 400-line cap is a file limit,
//! not a reason to invent a protocol between two owners of one invariant.
//!
//! The frame vocabulary and the limits all come from
//! `protocol/spec/sync.md`, and each constant here cites the v2 line it came
//! from.

pub mod ack_window;
pub mod admission;
pub mod commands;
pub mod control_frames;
pub mod domain_table;
pub mod egress;
pub mod frame_meta;
pub mod retained_frame;
pub mod send_queue;
pub mod session;
pub mod snapshot_registry;
pub mod terminal;
pub mod terminal_command;
pub mod upgrade_admission;

pub use admission::EnqueueOutcome;
pub use control_frames::ResetNotice;
pub use egress::{FlushStep, SendableFrame};
pub use domain_table::DomainGenerations;
pub use session::{SessionClose, SyncV2Session};
