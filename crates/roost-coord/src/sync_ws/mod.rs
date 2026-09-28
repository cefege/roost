//! The Sync WebSocket's per-connection v2 session: upgrade admission, the
//! cumulative delivery-sequence ACK window, the bounded frame queues, the
//! weighted-lane scheduler, the terminal fan-out, and the client command path.
//!
//! Owned by the coordinator. The session modules are pure -- no socket, no
//! clock of their own, no database -- because the admission ORDER, the queue
//! cutovers and the terminal fences are the properties worth testing and none
//! of them is observable through a live socket without flakiness. The I/O
//! shell around them lives here too: `socket` (the loop) and
//! `socket_open` (open and release), `driver` (the state a socket's listeners
//! and task share, and the flush turn), `live_feed` (the bus listeners),
//! `ingress` (client frames), with `v1_delivery` for sockets that did not
//! negotiate v2 and `resource_index` for what a socket may observe. What a
//! socket is owed besides live frames is `seed` (retained state, paced for v1
//! by `v1_seed`), `backfill` and `session_replay` (durable events above
//! `since`), and `open_sockets` is how a key revocation reaches it.
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
pub mod backfill;
pub mod commands;
pub mod commands_layout;
pub mod control_frames;
pub mod domain_table;
pub mod driver;
pub mod egress;
pub mod feed;
pub mod frame_meta;
pub mod ingress;
pub mod live_feed;
pub mod open_sockets;
pub mod resource_index;
pub mod retained_frame;
pub mod seed;
pub mod send_queue;
pub mod session;
pub mod session_replay;
pub mod snapshot_registry;
pub mod socket;
pub mod socket_open;
pub mod terminal;
pub mod terminal_command;
pub mod upgrade_admission;
pub mod v1_delivery;
pub mod v1_seed;

pub use admission::EnqueueOutcome;
pub use control_frames::ResetNotice;
pub use domain_table::DomainGenerations;
pub use egress::{FlushStep, SendableFrame};
pub use session::{SessionClose, SyncV2Session};
