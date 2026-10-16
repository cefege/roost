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
/// Hidden: restores every file a conversation's `write`/`edit` calls changed
/// at or after `checkpoint_id` (see `RewindFilesArgs`). Never offered to the model.
pub const TOOL_REWIND_FILES: &str = "rewind_files";
/// Background job inspection and cancellation (see `JobArgs`); a worker tool.
pub const TOOL_JOB: &str = "job";
/// Blocks until a background job settles or a message arrives; a harness tool.
pub const TOOL_WAIT: &str = "wait";

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
    /// `async: true` starts the command as a background job and returns its id at once.
    #[serde(default, rename = "async")]
    pub background: Option<bool>,
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
/// Arguments of the hidden `rewind_files` tool. Checkpoint ids are the
/// harness's user-entry sequence numbers, so "at or after" is numeric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RewindFilesArgs {
    pub checkpoint_id: u64,
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
            "Run a shell command with `bash -lc` in the conversation directory on the user's machine. `timeout` is in seconds (default 120, max 3600). `async: true` starts it as a background job and returns its id at once; a foreground command still running after 60 seconds is moved to the background automatically. A background result arrives as a follow-up when the command finishes: do not poll for it. Output streams live; the result keeps the last 50 KiB and names a file holding the full output. A nonzero exit is reported as `exit N`. Use it for builds, tests, git and real binaries, not for reading, searching or editing files.",
            json!({"type":"object","properties":{"command":{"type":"string"},"timeout":{"type":"integer","description":"Timeout in seconds"},"async":{"type":"boolean","description":"Run in the background and return the job id at once"}},"required":["command"],"additionalProperties":false}),
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
        spec(
            TOOL_JOB,
            "Inspect background jobs this conversation started. `list` shows every job and its state; `output` returns a job's output so far (`id` required); `cancel` stops a running job (`id` required). Job ids look like `bg_1`.",
            json!({"type":"object","properties":{"op":{"type":"string","enum":["list","output","cancel"]},"id":{"type":"string"}},"required":["op"],"additionalProperties":false}),
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

/// The argument every model-visible tool takes first: the concise intent the
/// working row shows while the call runs. The executor strips it before dispatch.
pub const INTENT_FIELD: &str = "i";
/// The longest intent the executor accepts; a longer one fails the call unrun.
pub const INTENT_MAX_CHARS: usize = 200;

/// `parameters` with the intent property added (first in `required`) once.
/// Idempotent, so specs built through several paths gain it exactly once.
pub fn with_intent_field(mut parameters: Value) -> Value {
    let Some(object) = parameters.as_object_mut() else {
        return parameters;
    };
    let properties = object.entry("properties").or_insert_with(|| json!({}));
    if let Some(properties) = properties.as_object_mut() {
        properties
            .entry(INTENT_FIELD)
            .or_insert_with(|| json!({"type": "string", "description": "concise intent"}));
    }
    let required = object.entry("required").or_insert_with(|| json!([]));
    if let Some(required) = required.as_array_mut()
        && !required.iter().any(|name| name == INTENT_FIELD)
    {
        required.insert(0, json!(INTENT_FIELD));
    }
    parameters
}

/// Why a call's intent was refused. The executor returns it as the call's
/// failure text, so the model sees the limit it broke.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntentError {
    #[error("Tool intent `i` is {chars} characters; the limit is {max}.")]
    TooLong { chars: usize, max: usize },
}

/// Splits the intent off a call's JSON arguments. Returns the intent (when a
/// nonblank string) and the arguments the tool receives. Arguments without an
/// intent come back byte-for-byte; an over-long intent is an error the call returns.
pub fn split_intent(args: &str) -> Result<(Option<String>, String), IntentError> {
    let Ok(Value::Object(mut object)) = serde_json::from_str::<Value>(args) else {
        return Ok((None, args.to_owned()));
    };
    let Some(raw) = object.remove(INTENT_FIELD) else {
        return Ok((None, args.to_owned()));
    };
    let intent = match raw {
        Value::String(text) => {
            let chars = text.chars().count();
            if chars > INTENT_MAX_CHARS {
                return Err(IntentError::TooLong {
                    chars,
                    max: INTENT_MAX_CHARS,
                });
            }
            intent_label(&text)
        }
        _ => None,
    };
    let dispatch =
        serde_json::to_string(&Value::Object(object)).unwrap_or_else(|_| args.to_owned());
    Ok((intent, dispatch))
}

/// The working-row label for an intent: trimmed, trailing dots dropped,
/// `None` when nothing is left. Shared by the executor and the web client.
pub fn intent_label(raw: &str) -> Option<String> {
    let label = raw.trim().trim_end_matches('.').trim_end();
    (!label.is_empty()).then(|| label.to_owned())
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

    #[test]
    fn every_worker_schema_takes_intent_first_and_required() {
        for spec in worker_tool_specs(false) {
            assert!(
                spec.parameters["properties"].get(INTENT_FIELD).is_none(),
                "{} takes `i` from toolset_for, not from the shared spec",
                spec.name
            );
            let parameters = with_intent_field(spec.parameters);
            assert_eq!(parameters["required"][0], INTENT_FIELD, "{}", spec.name);
            assert_eq!(
                parameters["properties"][INTENT_FIELD]["type"], "string",
                "{}",
                spec.name
            );
        }
        let once = with_intent_field(with_intent_field(
            json!({"type": "object", "properties": {}, "required": []}),
        ));
        assert_eq!(once["required"], json!([INTENT_FIELD]));
    }

    #[test]
    fn split_intent_strips_the_field_and_keeps_other_arguments() {
        let (intent, dispatch) =
            split_intent(r#"{"i":"Reading config...","path":"a.rs"}"#).expect("within limit");
        assert_eq!(intent.as_deref(), Some("Reading config"));
        assert_eq!(dispatch, r#"{"path":"a.rs"}"#);
    }

    #[test]
    fn split_intent_passes_arguments_without_intent_through_unchanged() {
        let args = r#"{ "path" : "a.rs" }"#;
        assert_eq!(split_intent(args), Ok((None, args.to_owned())));
        assert_eq!(split_intent("not json"), Ok((None, "not json".to_owned())));
    }

    #[test]
    fn split_intent_rejects_an_intent_over_the_limit() {
        let long = "a".repeat(INTENT_MAX_CHARS + 1);
        let args = json!({"i": long, "path": "a.rs"}).to_string();
        assert!(split_intent(&args).is_err());
        let at_limit = "a".repeat(INTENT_MAX_CHARS);
        let args = json!({"i": at_limit}).to_string();
        assert!(split_intent(&args).is_ok());
    }

    #[test]
    fn intent_label_trims_and_drops_trailing_dots() {
        assert_eq!(
            intent_label("  Running tests…  "),
            Some("Running tests…".into())
        );
        assert_eq!(intent_label("Searching..."), Some("Searching".into()));
        assert_eq!(intent_label("  ..  "), None);
    }
}
