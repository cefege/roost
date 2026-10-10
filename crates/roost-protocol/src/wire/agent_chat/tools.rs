//! Shared worker tool argument and model schema contracts.
//! Called by the coordinator to advertise tools and by workers to decode calls.
//! Depends only on serde and serde_json so every client can use the same shapes.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const TOOL_READ: &str = "read";
pub const TOOL_EDIT: &str = "edit";
pub const TOOL_WRITE: &str = "write";
pub const TOOL_BASH: &str = "bash";
pub const TOOL_GREP: &str = "grep";
pub const TOOL_GLOB: &str = "glob";
pub const TOOL_LSP: &str = "lsp";
pub const TOOL_CONTEXT_FILES: &str = "context_files";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadArgs {
    pub path: String,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditArgs {
    pub input: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteArgs {
    pub path: String,
    pub content: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BashArgs {
    pub command: String,
    #[serde(default)]
    pub timeout: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrepArgs {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub glob: Option<String>,
    #[serde(default)]
    pub case_insensitive: Option<bool>,
    #[serde(default)]
    pub context: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobArgs {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspAction {
    Diagnostics,
    Definition,
    References,
    Hover,
    Symbols,
    Rename,
    Status,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LspArgs {
    pub action: LspAction,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub new_name: Option<String>,
    #[serde(default)]
    pub apply: Option<bool>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextFiles {
    pub context: String,
    pub watchdog: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

pub fn worker_tool_specs(plan_mode: bool) -> Vec<WorkerToolSpec> {
    let mut specs = vec![
        spec(
            TOOL_READ,
            "Read a file or list a directory. A file comes back as a `[PATH#TAG]` header, then `N:<line>` rows; copy the header and line numbers into `edit`. `offset` is the 1-based first line, `limit` the line count; output stops at 2000 lines or 50 KiB with a note telling you the next offset. Read the ranges you need rather than whole large files.",
            json!({"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"}},"required":["path"],"additionalProperties":false}),
        ),
        spec(
            TOOL_BASH,
            "Run a shell command with `bash -lc` in the conversation directory on the user's machine. `timeout` is in seconds (default 120, max 3600). Output streams live; the result keeps the last 50 KiB and names a file holding the full output. A nonzero exit is reported as `exit N`. Use it for builds, tests, git and real binaries, not for reading, searching or editing files.",
            json!({"type":"object","properties":{"command":{"type":"string"},"timeout":{"type":"integer","description":"Timeout in seconds"}},"required":["command"],"additionalProperties":false}),
        ),
        spec(
            TOOL_GREP,
            "Search file contents for a regular expression (Rust regex syntax), respecting .gitignore. `path`: file or directory (default: the conversation directory); `glob`: limit to matching file names; `context`: lines shown around each match. Results are `[PATH#TAG]` headers with `N:<line>` rows usable by `edit`, capped at 200 matches.",
            json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"glob":{"type":"string"},"case_insensitive":{"type":"boolean"},"context":{"type":"integer"}},"required":["pattern"],"additionalProperties":false}),
        ),
        spec(
            TOOL_GLOB,
            "List files matching a glob pattern (e.g. `**/*.rs`, `src/*.ts`) under `path` (default: the conversation directory), respecting .gitignore, newest first, at most 500 paths.",
            json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"],"additionalProperties":false}),
        ),
        spec(
            TOOL_LSP,
            "Ask the project's language server. `diagnostics` for `file` (or `file: \"*\"` to run the workspace checkers: cargo check, tsc, ruff); `definition`, `references` and `hover` at `line` of `file`, with `symbol` naming the identifier on that line; `symbols` outlines `file`; `rename` plans (or with `apply: true` applies) renaming `symbol` to `new_name`; `status` lists running servers.",
            lsp_schema(plan_mode),
        ),
    ];
    if !plan_mode {
        specs.push(spec(TOOL_EDIT, include_str!("prompts/hashline.md"), json!({"type":"object","properties":{"input":{"type":"string","description":"Hashline edit instructions"}},"required":["input"],"additionalProperties":false})));
        specs.push(spec(TOOL_WRITE, "Create a file or replace its entire content, creating parent directories. Returns the new `[PATH#TAG]` header and language-server diagnostics. Use `edit` for changes to an existing file.", json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false})));
    }
    specs
}

fn spec(name: &str, description: &str, parameters: Value) -> WorkerToolSpec {
    WorkerToolSpec {
        name: name.into(),
        description: description.into(),
        parameters,
    }
}
fn lsp_schema(plan_mode: bool) -> Value {
    let actions: &[&str] = if plan_mode {
        &[
            "diagnostics",
            "definition",
            "references",
            "hover",
            "symbols",
        ]
    } else {
        &[
            "diagnostics",
            "definition",
            "references",
            "hover",
            "symbols",
            "rename",
            "status",
        ]
    };
    json!({"type":"object","properties":{"action":{"type":"string","enum":actions},"file":{"type":"string"},"line":{"type":"integer"},"symbol":{"type":"string"},"new_name":{"type":"string"},"apply":{"type":"boolean"}},"required":["action"],"additionalProperties":false})
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn plan_mode_removes_mutating_tools_and_limits_lsp() {
        let specs = worker_tool_specs(true);
        assert!(
            !specs
                .iter()
                .any(|tool| tool.name == TOOL_EDIT || tool.name == TOOL_WRITE)
        );
        let lsp = specs
            .iter()
            .find(|tool| tool.name == TOOL_LSP)
            .expect("LSP tool exists");
        assert_eq!(
            lsp.parameters["properties"]["action"]["enum"],
            json!([
                "diagnostics",
                "definition",
                "references",
                "hover",
                "symbols"
            ])
        );
    }
}
