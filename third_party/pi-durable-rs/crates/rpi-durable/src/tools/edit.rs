//! Edit tool (port of `packages/durable/src/tools/edit.ts` @ `abe508e1b`).

use std::sync::Arc;

use async_trait::async_trait;
use rpi_ai::types::{TextContent, Tool, ToolResultContent};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::chord::context::Context;
use crate::chord::json::JsonObject;
use crate::env::types::{FileContent, FileKind};
use crate::errors::{FileError, FileErrorCode};
use crate::harness::types::{
    ToolExecute, ToolExecuteError, ToolExecutionApi, ToolExecutionResult, ToolRegistration,
    ToolReplay,
};
use crate::tools::edit_diff::{
    Edit, apply_edits_to_normalized_content, detect_line_ending, generate_diff_string,
    generate_unified_patch, normalize_to_lf, restore_line_endings, strip_bom,
};
use crate::tools::env::require_env;
use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::path_utils::resolve_tool_path;
use crate::tools::tool_error;

/// Details of one edit (`EditToolDetails`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EditToolDetails {
    /// Display-oriented diff with line numbers.
    pub diff: String,
    /// Standard unified patch.
    pub patch: String,
    /// First changed line in the new file, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<usize>,
}

fn is_single_edit_input(value: &Value) -> bool {
    value
        .as_object()
        .map(|object| {
            object.get("oldText").is_some_and(Value::is_string)
                && object.get("newText").is_some_and(Value::is_string)
        })
        .unwrap_or(false)
}

/// Repair shapes models commonly send: `edits` as a JSON string or as a single
/// edit object, and a top-level `oldText`/`newText` pair. Works on a copy; the
/// call's arguments stay unchanged.
pub fn prepare_edit_arguments(input: &Value) -> Result<Value, String> {
    let Some(object) = input.as_object() else {
        return Ok(input.clone());
    };
    let mut args = object.clone();
    match args.get("edits") {
        Some(Value::String(text)) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                if parsed.is_array() {
                    args.insert("edits".to_string(), parsed);
                } else if is_single_edit_input(&parsed) {
                    args.insert("edits".to_string(), Value::Array(vec![parsed]));
                }
            }
        }
        Some(edits) if !edits.is_array() && is_single_edit_input(edits) => {
            let single = edits.clone();
            args.insert("edits".to_string(), Value::Array(vec![single]));
        }
        _ => {}
    }

    let legacy_old = args
        .get("oldText")
        .and_then(Value::as_str)
        .map(str::to_string);
    let legacy_new = args
        .get("newText")
        .and_then(Value::as_str)
        .map(str::to_string);
    let (Some(old_text), Some(new_text)) = (legacy_old, legacy_new) else {
        return Ok(Value::Object(args));
    };
    let mut edits: Vec<Value> = args
        .get("edits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    edits.push(json!({ "oldText": old_text, "newText": new_text }));
    args.remove("oldText");
    args.remove("newText");
    args.insert("edits".to_string(), Value::Array(edits));
    Ok(Value::Object(args))
}

fn edit_access_error(path: &str, error: &FileError) -> ToolExecuteError {
    tool_error(format!(
        "Could not edit file: {path}. Error code: {}.",
        error_code_text(error.code)
    ))
}

fn error_code_text(code: FileErrorCode) -> &'static str {
    match code {
        FileErrorCode::Aborted => "aborted",
        FileErrorCode::NotFound => "not_found",
        FileErrorCode::PermissionDenied => "permission_denied",
        FileErrorCode::NotDirectory => "not_directory",
        FileErrorCode::IsDirectory => "is_directory",
        FileErrorCode::Invalid => "invalid",
        FileErrorCode::NotSupported => "not_supported",
        FileErrorCode::Unknown => "unknown",
    }
}

fn aborted(context: &Context) -> bool {
    context
        .abort_signal()
        .map(|signal| signal.is_cancelled())
        .unwrap_or(false)
}

struct EditTool;

#[async_trait]
impl ToolExecute for EditTool {
    async fn execute(
        &self,
        args: JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let raw_edits = args
            .get("edits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if raw_edits.is_empty() {
            return Err(tool_error(
                "Edit tool input is invalid. edits must contain at least one replacement.",
            ));
        }
        let edits: Vec<Edit> = raw_edits
            .iter()
            .map(|edit| Edit {
                old_text: edit
                    .get("oldText")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                new_text: edit
                    .get("newText")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect();
        let env = require_env(api)?;
        let absolute = resolve_tool_path(env.as_ref(), &path, context).await?;
        let block_count = edits.len();
        with_file_mutation_queue(Arc::clone(&env), &absolute, context, async {
            if aborted(context) {
                return Err(tool_error("Operation aborted"));
            }
            let info = env
                .file_info(&absolute, context)
                .await
                .map_err(|error| edit_access_error(&path, &error))?;
            if info.kind != FileKind::File && info.kind != FileKind::Symlink {
                return Err(tool_error(format!(
                    "Could not edit file: {path}. Path is not a file."
                )));
            }

            let read_result = env
                .read_text_file(&absolute, context)
                .await
                .map_err(|error| edit_access_error(&path, &error))?;
            if aborted(context) {
                return Err(tool_error("Operation aborted"));
            }

            let (bom, content) = strip_bom(&read_result);
            let original_ending = detect_line_ending(&content);
            let normalized_content = normalize_to_lf(&content);
            let applied = apply_edits_to_normalized_content(&normalized_content, &edits, &path)
                .map_err(tool_error)?;
            if aborted(context) {
                return Err(tool_error("Operation aborted"));
            }

            let final_content = format!(
                "{bom}{}",
                restore_line_endings(&applied.new_content, original_ending)
            );
            env.write_file(&absolute, FileContent::Text(final_content), context)
                .await
                .map_err(|error| edit_access_error(&path, &error))?;
            if aborted(context) {
                return Err(tool_error("Operation aborted"));
            }

            let diff_result = generate_diff_string(&applied.base_content, &applied.new_content, 4);
            let details = EditToolDetails {
                diff: diff_result.diff,
                patch: generate_unified_patch(
                    &path,
                    &applied.base_content,
                    &applied.new_content,
                    4,
                ),
                first_changed_line: diff_result.first_changed_line,
            };
            Ok(ToolExecutionResult {
                content: Some(vec![ToolResultContent::Text(TextContent {
                    text: format!("Successfully replaced {block_count} block(s) in {path}."),
                    text_signature: None,
                })]),
                details: Some(
                    serde_json::to_value(details).map_err(|error| tool_error(error.to_string()))?,
                ),
                ..Default::default()
            })
        })
        .await
    }
}

fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the file to edit (relative or absolute)"
            },
            "edits": {
                "type": "array",
                "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                "items": {
                    "type": "object",
                    "properties": {
                        "oldText": {
                            "type": "string",
                            "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."
                        },
                        "newText": {
                            "type": "string",
                            "description": "Replacement text for this targeted edit."
                        }
                    },
                    "required": ["oldText", "newText"]
                }
            }
        },
        "required": ["path", "edits"]
    })
}

/// `createEditTool` (edit.ts:90).
pub fn create_edit_tool() -> ToolRegistration {
    ToolRegistration {
        tool: Tool {
            name: "edit".to_string(),
            description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".to_string(),
            parameters: parameters(),
            constrained_sampling: None,
        },
        replay: ToolReplay::Unsafe,
        execution_mode: None,
        deferred: false,
        prepare_arguments: Some(Arc::new(|input: &Value| prepare_edit_arguments(input))),
        output_limits: None,
        structured_output_schema: None,
        callers: None,
        execute: Arc::new(EditTool),
    }
}
