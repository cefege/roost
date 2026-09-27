//! The coordinator service: SQLite state, auth, Connect RPC handlers, the Sync
//! and worker WebSocket links, terminal hubs, and web push. Speaks the wire,
//! never the terminal.
//!
//! Owned here; [`serve`] is the entry point a process calls and it blocks until
//! the process is asked to stop. The contract every surface in this crate upholds
//! -- the schema, the admission order, the close codes, the bounds and the reason
//! each one is that number -- is written down in
//! `docs/phase3-coord-contract.md`, and each constant here cites it.
//!
//! The shape of the crate is: a **pure core** and a **thin I/O shell**. The pure
//! core is `auth::jwt_claims`, `auth::jwt_crypto`, `auth::jwt_verify`,
//! `auth::principal`, `write_gate`, `events::admission`,
//! `events::pending_publications`, `worker_link::*` and `sync_ws::*` -- none of
//! which opens a socket, reads a clock of its own, or touches a database. The
//! shell is `db`, `auth::authorized_keys`, `auth::authenticate`, `http::listener`
//! and `serve`. That split is why the ordering rules are testable at all, and it
//! is the same split `roost-worker` uses.

#![forbid(unsafe_code)]

pub mod agents;
pub mod attachments;
pub mod auth;
pub mod coord_core;
pub mod db;
pub mod deploy;
pub mod diagnostics;
pub mod events;
pub mod http;
pub mod http_admission;
pub mod maintenance;
pub mod middleware;
pub mod push;
pub mod rpc;
pub mod search;
pub mod serve;
pub mod services;
pub mod sessions;
pub mod sync_ws;
pub mod terminal_screen;
pub mod terminal_view;
pub mod ui_state;
pub mod worker_link;
pub mod workers;
pub mod write_gate;

pub use serve::{CoordBoot, serve};
