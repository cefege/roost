//! Ported from oh-my-pi packages/coding-agent/src/lsp/ (MIT).
//! This module exposes the worker's language-server manager and installer.
//! Server, client, document, action and formatting concerns live in sibling modules.

mod actions;
mod client;
mod documents;
mod edits;
pub mod install;
mod manager;
mod render;
mod servers;
mod types;
mod uri;
mod workspace_checks;

pub use manager::LspManager;
