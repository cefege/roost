//! The coordinator methods UI surfaces call directly, one file per domain,
//! each request a `unary::UnaryMethod` with Rust-typed answers.
//!
//! Called through roost-web's `CoordRpc::call`; encoded with the shared
//! `codec::{encode_message, decode_message}`. v2's equivalent is every
//! `coordClient.<method>(…)` call site under `apps/web/src/`.

pub mod sessions;
pub mod ui_state;
pub mod workspaces;
