//! The coordinator methods UI surfaces call directly, one file per domain,
//! each request a `unary::UnaryMethod` with Rust-typed answers.
//!
//! Called through roost-web's `CoordRpc::call`; encoded with the shared
//! `codec::{encode_message, decode_message}`. v2's equivalent is every
//! `coordClient.<method>(…)` call site under `apps/web/src/`.

pub mod agent_chat;
pub mod attachment_direct;
pub mod attachments;
pub mod browse;
pub mod clipboard;
pub mod diagnostics;
pub mod files;
pub mod find;
pub mod pairing;
pub mod sessions;
pub mod settings;
pub mod tasks;
pub mod terminal_image;
pub mod terminal_pane;
pub mod ui_state;
pub mod workspaces;
