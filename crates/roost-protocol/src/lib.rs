//! Pure, I/O-free wire and terminal logic: the event fold, the cell and grid
//! models, the chunk assembler, viewport geometry, peer packet framing, the
//! keeper-update contract, and layout documents.
//!
//! Builds for `wasm32-unknown-unknown` as well as native, so the browser, the
//! coordinator, the worker and every future front end run the same code. That
//! is the whole reason this crate exists: one event fold, one chunk
//! assembler, one set of limits. A second copy in a client or a server is the
//! defect `docs/FAILURE-INDEX.md` records and this crate's existence prevents.
//!
//! What this crate must never do: read a clock, touch the filesystem, spawn a
//! socket, or name a platform. Where the TypeScript original defaulted to
//! `Date.now()` or `performance.now()`, the port takes the timestamp as a
//! parameter, so the caller owns time and the tests are deterministic.

#![forbid(unsafe_code)]

pub mod cell;
pub mod error;
pub mod json;
pub mod validate;
pub mod versioning;
pub mod wire;

pub mod agent_conversation_reference;
pub mod fingerprint;
pub mod keeper_update;
pub mod layout;
pub mod local_ui_door;
pub mod proto_adapters;
pub mod terminal_capture;
pub mod terminal_input;
pub mod terminal_peer;
pub mod terminal_search;
pub mod viewport;

pub use error::{ProtocolError, ProtocolResult};
