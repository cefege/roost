//! The worker-side agent tools: read, write, hashline edit, bash, grep, glob,
//! LSP and project context. The worker runtime calls `ToolHost`; argument shapes
//! come from roost-protocol and language services are supplied by its LSP module.

#![forbid(unsafe_code)]

pub mod bash;
pub mod context_files;
pub mod file_tools;
pub mod glob;
pub mod grep;
pub mod hashline;
pub mod host;
pub mod lsp;
pub mod outcome;
pub mod search_path;

pub use host::{ToolCallRequest, ToolHost};
pub use outcome::ToolOutcome;
