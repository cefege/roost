//! The worker-side agent tools: read, write, hashline edit, bash, grep, glob,
//! LSP and project context. The worker runtime calls `ToolHost`; argument shapes
//! come from roost-protocol and language services are supplied by its LSP module.

#![forbid(unsafe_code)]

pub mod bash;
mod bash_capture;
mod bash_process;
pub mod context_files;
mod file_mutations;
pub mod file_tools;
pub mod glob;
pub mod grep;
pub mod hashline;
pub mod host;
mod jobs;
pub mod lsp;
pub mod outcome;
pub mod restore_points;
pub mod search_path;

pub use host::{ToolCallRequest, ToolHost};
pub use jobs::SettledJob;
pub use outcome::ToolOutcome;
