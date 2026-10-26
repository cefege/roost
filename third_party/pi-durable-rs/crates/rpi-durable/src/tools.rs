//! Durable tool set (port of `packages/durable/src/tools/*` @ `abe508e1b`).
//!
//! The module carries the upstream `read`/`write`/`edit`/`bash` tools and the
//! `powershell` tool, the environment injection point, the file-mutation queue
//! and the shared path/image helpers. `CodingTools` — `read`, `write`, `edit`
//! and `bash` — is an [`crate::harness::types::Extension`]; nothing installs it
//! automatically (upstream `tools/index.ts:22`).

pub mod bash;
pub mod edit;
pub mod edit_diff;
pub mod env;
pub mod file_mutation_queue;
pub mod image;
pub mod path_utils;
pub mod read;
pub mod write;

pub use bash::{
    BashExecution, BashPrepare, BashToolOptions, PowerShellToolOptions, create_bash_tool,
    create_power_shell_tool,
};
pub use edit::{EditToolDetails, create_edit_tool, prepare_edit_arguments};
pub use edit_diff::{
    AppliedEditsResult, DiffResult, Edit, apply_edits_to_normalized_content, detect_line_ending,
    generate_diff_string, generate_unified_patch, normalize_for_fuzzy_match, normalize_to_lf,
    restore_line_endings, strip_bom,
};
pub use env::require_env;
pub use file_mutation_queue::with_file_mutation_queue;
pub use image::{
    ByteSource, detect_supported_image_mime_type, detect_supported_image_mime_type_of,
};
pub use path_utils::{resolve_read_tool_path, resolve_tool_path};
pub use read::{ReadToolDetails, create_read_tool};
pub use write::create_write_tool;

use std::sync::Arc;

use crate::harness::define::define_extension;
use crate::harness::types::{Extension, ToolExecuteError};

/// Ordinary tool failure carrying the upstream `Error.message` text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ToolError(pub String);

/// Build a [`ToolExecuteError`] with the upstream message.
pub fn tool_error(message: impl Into<String>) -> ToolExecuteError {
    Box::new(ToolError(message.into()))
}

/// `read`, `write`, `edit` and `bash`; nothing installs it automatically.
/// `create_power_shell_tool()` adds `powershell`.
pub fn coding_tools() -> Extension {
    define_extension(Extension {
        name: "coding-tools".to_string(),
        tools: vec![
            create_read_tool(),
            create_write_tool(),
            create_edit_tool(),
            create_bash_tool(None),
        ],
        ..Default::default()
    })
}

/// Shared helper: an extension with one tool (used by the powershell variant
/// and tests).
pub fn single_tool_extension(
    name: &str,
    tool: crate::harness::types::ToolRegistration,
) -> Extension {
    define_extension(Extension {
        name: name.to_string(),
        tools: vec![tool],
        ..Default::default()
    })
}

/// A boxed environment handle (upstream `ExecutionEnv` is passed by reference).
pub type SharedEnv = Arc<dyn crate::env::types::ExecutionEnv>;
