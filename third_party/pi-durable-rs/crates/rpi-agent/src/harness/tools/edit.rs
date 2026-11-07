//! Port of `packages/agent/src/harness/tools/edit.ts` @ pi 0.82.1 (2efa728) —
//! the `edit` tool: exact-text replacement with legacy-argument shims, BOM /
//! line-ending preservation, and diff details.
//!
//! Intentional differences:
//! - `prepareEditArguments` (edit.ts:48-64) operates on `serde_json::Value`
//!   (the trait's `prepare_arguments` shim) instead of a typed record.
//! - `editAccessError` (edit.ts:73-75) becomes `AgentError::Message` with the
//!   upstream text, including the `FileError.code` literal.
//! - The `EditToolDetails` (edit.ts:42-46) fields are rendered into the
//!   `AgentToolResult.details` JSON directly (camelCase keys: `diff`,
//!   `patch`, `firstChangedLine`; the line is omitted when absent, like an
//!   upstream `undefined`).
//! - `AbortSignal | undefined` is `CancellationToken`; the four abort checks
//!   (edit.ts:93, 102, 108, 113) throw inside the mutation queue.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::harness::tools::edit_diff::{
    Edit, apply_edits_to_normalized_content, detect_line_ending, generate_diff_string,
    generate_unified_patch, normalize_to_lf, restore_line_endings, strip_bom,
};
use crate::harness::tools::file_mutation_queue::with_file_mutation_queue;
use crate::harness::tools::path_utils::resolve_tool_path;
use crate::harness::tools::tool_context::ToolContext;
use crate::harness::types::{AgentHarnessTool, FileError, FileKind};
use crate::types::{AgentToolResult, AgentToolUpdateCallback};

/// `EditToolInput` (edit.ts:28-37).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EditToolInput {
    pub path: String,
    /// `edits` is kept as a raw `Value` so the "not an array" case surfaces the
    /// upstream validation message (edit.ts:66-72) instead of a serde error.
    #[serde(default)]
    pub edits: Value,
}

/// `EditToolDetails` (edit.ts:42-46).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditToolDetails {
    pub diff: String,
    pub patch: String,
    pub first_changed_line: Option<usize>,
}

impl EditToolDetails {
    /// Render into the result `details` JSON (camelCase keys; `firstChangedLine`
    /// omitted when `None`, like upstream `undefined`).
    fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("diff".into(), Value::String(self.diff.clone()));
        map.insert("patch".into(), Value::String(self.patch.clone()));
        if let Some(line) = self.first_changed_line {
            map.insert("firstChangedLine".into(), Value::from(line));
        }
        Value::Object(map)
    }
}

/// `prepareEditArguments` (edit.ts:48-64): unwrap a JSON-string `edits` and
/// merge legacy top-level `oldText` / `newText` into the edits array.
fn prepare_edit_arguments(args: Value) -> Value {
    let Value::Object(mut map) = args else {
        return args;
    };
    if let Some(Value::String(s)) = map.get("edits")
        && let Ok(parsed) = serde_json::from_str::<Value>(s)
        && parsed.is_array()
    {
        map.insert("edits".into(), parsed);
    }

    let old_text = map.get("oldText").cloned();
    let new_text = map.get("newText").cloned();
    if !(matches!(&old_text, Some(Value::String(_))) && matches!(&new_text, Some(Value::String(_))))
    {
        return Value::Object(map);
    }
    let mut edits: Vec<Value> = match map.get("edits") {
        Some(Value::Array(array)) => array.clone(),
        _ => Vec::new(),
    };
    edits.push(json!({ "oldText": old_text, "newText": new_text }));
    map.remove("oldText");
    map.remove("newText");
    map.insert("edits".into(), Value::Array(edits));
    Value::Object(map)
}

/// `validateEditInput` (edit.ts:66-72) — the "not an array" and "empty array"
/// cases share one upstream message.
fn validate_edit_input(input: &EditToolInput) -> Result<Vec<Edit>, AgentError> {
    let edits = match &input.edits {
        Value::Array(items) => items
            .iter()
            .cloned()
            .map(|item| serde_json::from_value::<Edit>(item).map_err(AgentError::Json))
            .collect::<Result<Vec<Edit>, AgentError>>()?,
        _ => {
            return Err(AgentError::Message(
                "Edit tool input is invalid. edits must contain at least one replacement."
                    .to_string(),
            ));
        }
    };
    if edits.is_empty() {
        return Err(AgentError::Message(
            "Edit tool input is invalid. edits must contain at least one replacement.".to_string(),
        ));
    }
    Ok(edits)
}

/// `editAccessError` (edit.ts:73-75).
fn edit_access_error(path: &str, error: &FileError) -> AgentError {
    AgentError::Message(format!(
        "Could not edit file: {path}. Error code: {}.",
        error.code.as_str()
    ))
}

/// The `edit` tool (edit.ts:77-126).
pub struct EditTool {
    description: String,
    parameters: Value,
}

/// `createEditTool` (edit.ts:77).
pub fn create_edit_tool() -> EditTool {
    EditTool {
        description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes."
            .to_string(),
        parameters: json!({
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
        }),
    }
}

#[async_trait]
impl<TContext: ToolContext> AgentHarnessTool<TContext> for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn label(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> &Value {
        &self.parameters
    }

    fn prepare_arguments(&self, args: Value) -> Value {
        prepare_edit_arguments(args)
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        signal: CancellationToken,
        _on_update: Option<AgentToolUpdateCallback>,
        context: TContext,
    ) -> Result<AgentToolResult, AgentError> {
        let input: EditToolInput = serde_json::from_value(params).map_err(AgentError::Json)?;
        let edits = validate_edit_input(&input)?;
        let env = context.env();
        let absolute_path =
            resolve_tool_path(env.as_ref(), &input.path, Some(signal.clone())).await?;
        let env_for_queue = Arc::clone(&env);
        let path_for_queue = absolute_path.clone();
        with_file_mutation_queue(env.as_ref(), &absolute_path, async move {
            if signal.is_cancelled() {
                return Err(AgentError::Message("Operation aborted".to_string()));
            }
            let info = env_for_queue
                .file_info(&path_for_queue, Some(signal.clone()))
                .await
                .map_err(|error| edit_access_error(&input.path, &error))?;
            if info.kind != FileKind::File && info.kind != FileKind::Symlink {
                return Err(AgentError::Message(format!(
                    "Could not edit file: {}. Path is not a file.",
                    input.path
                )));
            }

            let read_result = env_for_queue
                .read_text_file(&path_for_queue, Some(signal.clone()))
                .await
                .map_err(|error| edit_access_error(&input.path, &error))?;
            if signal.is_cancelled() {
                return Err(AgentError::Message("Operation aborted".to_string()));
            }

            let (bom, content) = strip_bom(&read_result);
            let original_ending = detect_line_ending(&content);
            let normalized_content = normalize_to_lf(&content);
            let applied =
                apply_edits_to_normalized_content(&normalized_content, &edits, &input.path)?;
            if signal.is_cancelled() {
                return Err(AgentError::Message("Operation aborted".to_string()));
            }

            let final_content = format!(
                "{bom}{}",
                restore_line_endings(&applied.new_content, original_ending)
            );
            env_for_queue
                .write_file(
                    &path_for_queue,
                    final_content.as_bytes(),
                    Some(signal.clone()),
                )
                .await
                .map_err(|error| edit_access_error(&input.path, &error))?;
            if signal.is_cancelled() {
                return Err(AgentError::Message("Operation aborted".to_string()));
            }

            let diff_result = generate_diff_string(&applied.base_content, &applied.new_content, 4);
            let details = EditToolDetails {
                diff: diff_result.diff,
                patch: generate_unified_patch(
                    &input.path,
                    &applied.base_content,
                    &applied.new_content,
                    4,
                ),
                first_changed_line: diff_result.first_changed_line,
            };
            Ok(AgentToolResult {
                content: vec![crate::harness::tools::text_content(format!(
                    "Successfully replaced {} block(s) in {}.",
                    edits.len(),
                    input.path
                ))],
                details: details.to_value(),
                ..Default::default()
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use serde_json::Value;

    use super::*;
    use crate::harness::env::nodejs::NodeExecutionEnv;
    use crate::harness::tools::ExecutionToolContext;
    use crate::harness::tools::test_helpers::{TempDir, ToolEnv, apply_unified_patch, text_output};
    use crate::harness::types::ExecutionEnv;

    #[tokio::test]
    async fn applies_disjoint_edits_and_returns_both_diff_formats() {
        // "applies disjoint edits and returns both diff formats"
        // (tools.test.ts:288-312).
        let dir = TempDir::new();
        let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.cwd()));
        let original = "alpha\nbeta\ngamma\ndelta\n";
        env.write_file("edit.txt", original.as_bytes(), None)
            .await
            .unwrap();

        let result = create_edit_tool()
            .execute(
                "edit-1",
                serde_json::json!({
                    "path": "edit.txt",
                    "edits": [
                        { "oldText": "alpha\n", "newText": "ALPHA\n" },
                        { "oldText": "gamma\n", "newText": "GAMMA\n" }
                    ]
                }),
                CancellationToken::new(),
                None,
                ExecutionToolContext::new(Arc::clone(&env)),
            )
            .await
            .unwrap();

        assert_eq!(
            text_output(&result),
            "Successfully replaced 2 block(s) in edit.txt."
        );
        let diff = result.details["diff"].as_str().unwrap();
        assert!(diff.contains("ALPHA"));
        assert!(diff.contains("GAMMA"));
        let patch = result.details["patch"].as_str().unwrap();
        assert_eq!(
            apply_unified_patch(original, patch),
            "ALPHA\nbeta\nGAMMA\ndelta\n"
        );
        let content = env.read_text_file("edit.txt", None).await.unwrap();
        assert_eq!(content, "ALPHA\nbeta\nGAMMA\ndelta\n");
    }

    #[tokio::test]
    async fn matches_all_edits_against_original_and_rejects_overlaps() {
        // "matches all edits against the original and rejects overlaps"
        // (tools.test.ts:314-334).
        let dir = TempDir::new();
        let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.cwd()));
        env.write_file("edit.txt", b"one\ntwo\nthree\n", None)
            .await
            .unwrap();

        let err = create_edit_tool()
            .execute(
                "edit-2",
                serde_json::json!({
                    "path": "edit.txt",
                    "edits": [
                        { "oldText": "one\ntwo\n", "newText": "ONE\nTWO\n" },
                        { "oldText": "two\nthree\n", "newText": "TWO\nTHREE\n" }
                    ]
                }),
                CancellationToken::new(),
                None,
                ExecutionToolContext::new(Arc::clone(&env)),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("overlap"));
        let content = env.read_text_file("edit.txt", None).await.unwrap();
        assert_eq!(content, "one\ntwo\nthree\n");
    }

    #[tokio::test]
    async fn rejects_missing_and_duplicate_target_text() {
        // "rejects missing and duplicate target text" (tools.test.ts:336-359).
        let dir = TempDir::new();
        let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.cwd()));
        env.write_file("edit.txt", b"foo foo foo", None)
            .await
            .unwrap();
        let tool = create_edit_tool();

        let err = tool
            .execute(
                "edit-3",
                serde_json::json!({ "path": "edit.txt", "edits": [{ "oldText": "bar", "newText": "baz" }] }),
                CancellationToken::new(),
                None,
                ExecutionToolContext::new(Arc::clone(&env)),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Could not find the exact text"));

        let err = tool
            .execute(
                "edit-4",
                serde_json::json!({ "path": "edit.txt", "edits": [{ "oldText": "foo", "newText": "bar" }] }),
                CancellationToken::new(),
                None,
                ExecutionToolContext::new(Arc::clone(&env)),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Found 3 occurrences"));
    }

    #[tokio::test]
    async fn keeps_mutation_queue_locked_until_aborted_edit_write_settles() {
        // "keeps the mutation queue locked until an aborted edit write settles"
        // (tools.test.ts:361-390).
        let dir = TempDir::new();
        let first_write_started = Arc::new(tokio::sync::watch::channel(false).0);
        let (finish_first_write_tx, finish_first_write_rx) = tokio::sync::watch::channel(false);
        let second_write_started = Arc::new(AtomicBool::new(false));

        let env: Arc<dyn ExecutionEnv> = Arc::new(
            ToolEnv::new(NodeExecutionEnv::new(dir.cwd())).with_write_hook(Arc::new({
                let first_write_started = Arc::clone(&first_write_started);
                let second_write_started = Arc::clone(&second_write_started);
                let finish_first_write_rx = finish_first_write_rx.clone();
                move |content: &[u8], _abort_signal| {
                    let first_write_started = Arc::clone(&first_write_started);
                    let second_write_started = Arc::clone(&second_write_started);
                    let mut finish_first_write_rx = finish_first_write_rx.clone();
                    Box::pin(async move {
                        if content == b"ALPHA\nbeta\n" {
                            let _ = first_write_started.send(true);
                            // Block until the test releases the first edit write
                            // (BlockingEditExecutionEnv, tools.test.ts:73-81).
                            let _ = finish_first_write_rx.wait_for(|v| *v).await;
                            // Upstream writes without the abort signal
                            // (tools.test.ts:81), so the write itself succeeds
                            // and the tool's own abort check throws
                            // "Operation aborted".
                            return crate::harness::tools::test_helpers::WriteFileAction::IgnoreAbortSignal;
                        }
                        if content == b"ALPHA\nBETA\n" || content == b"alpha\nBETA\n" {
                            second_write_started.store(true, Ordering::SeqCst);
                        }
                        crate::harness::tools::test_helpers::WriteFileAction::Continue
                    })
                }
            })),
        );
        env.write_file("file.txt", b"alpha\nbeta\n", None)
            .await
            .unwrap();

        let tool = Arc::new(create_edit_tool());
        let signal = CancellationToken::new();
        // Spawned: Rust futures are lazy (see the write queue test).
        let first_edit = {
            let tool = Arc::clone(&tool);
            let env = Arc::clone(&env);
            let signal = signal.clone();
            tokio::spawn(async move {
                tool.execute(
                    "edit-first",
                    serde_json::json!({ "path": "file.txt", "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }),
                    signal,
                    None,
                    ExecutionToolContext::new(env),
                )
                .await
            })
        };
        let mut started_rx = first_write_started.subscribe();
        started_rx.wait_for(|v| *v).await.unwrap();
        signal.cancel();
        let second_edit = {
            let tool = Arc::clone(&tool);
            let env = Arc::clone(&env);
            tokio::spawn(async move {
                tool.execute(
                    "edit-second",
                    serde_json::json!({ "path": "file.txt", "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
                    CancellationToken::new(),
                    None,
                    ExecutionToolContext::new(env),
                )
                .await
            })
        };

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !second_write_started.load(Ordering::SeqCst),
            "second edit must stay queued while the first is aborted"
        );
        let _ = finish_first_write_tx.send(true);
        let first_err = first_edit.await.unwrap().unwrap_err();
        assert!(first_err.to_string().contains("Operation aborted"));
        second_edit.await.unwrap().unwrap();
        let content = env.read_text_file("file.txt", None).await.unwrap();
        assert_eq!(content, "ALPHA\nBETA\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn serializes_concurrent_edits_through_canonical_and_symlink_paths() {
        // "serializes concurrent edits through canonical and symlink paths"
        // (tools.test.ts:392-416).
        use std::os::unix::fs::symlink;
        use std::path::Path;

        let dir = TempDir::new();
        let cwd = dir.path().to_string_lossy().into_owned();
        let env: Arc<dyn ExecutionEnv> = Arc::new(
            ToolEnv::new(NodeExecutionEnv::new(&cwd))
                .with_read_delay(std::time::Duration::from_millis(20)),
        );
        env.write_file("target.txt", b"alpha\nbeta\ngamma\n", None)
            .await
            .unwrap();
        symlink("target.txt", Path::new(&cwd).join("link.txt")).unwrap();
        let tool = create_edit_tool();

        let (r1, r2) = tokio::join!(
            tool.execute(
                "edit-target",
                serde_json::json!({ "path": "target.txt", "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }),
                CancellationToken::new(),
                None,
                ExecutionToolContext::new(Arc::clone(&env)),
            ),
            tool.execute(
                "edit-link",
                serde_json::json!({ "path": "link.txt", "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
                CancellationToken::new(),
                None,
                ExecutionToolContext::new(Arc::clone(&env)),
            ),
        );
        r1.unwrap();
        r2.unwrap();

        let content = env.read_text_file("target.txt", None).await.unwrap();
        assert_eq!(content, "ALPHA\nBETA\ngamma\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edits_regular_files_through_symlinks() {
        // "edits regular files through symlinks" (tools.test.ts:418-432).
        use std::os::unix::fs::symlink;
        use std::path::Path;

        let dir = TempDir::new();
        let cwd = dir.path().to_string_lossy().into_owned();
        let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(&cwd));
        env.write_file("target.txt", b"before\n", None)
            .await
            .unwrap();
        symlink("target.txt", Path::new(&cwd).join("link.txt")).unwrap();

        create_edit_tool()
            .execute(
                "edit-symlink",
                serde_json::json!({ "path": "link.txt", "edits": [{ "oldText": "before", "newText": "after" }] }),
                CancellationToken::new(),
                None,
                ExecutionToolContext::new(Arc::clone(&env)),
            )
            .await
            .unwrap();

        let content = env.read_text_file("target.txt", None).await.unwrap();
        assert_eq!(content, "after\n");
    }

    #[tokio::test]
    async fn preserves_bom_and_crlf_line_endings() {
        // "preserves BOM and CRLF line endings" (tools.test.ts:434-447).
        let dir = TempDir::new();
        let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.cwd()));
        env.write_file("edit.txt", "\u{FEFF}one\r\ntwo\r\n".as_bytes(), None)
            .await
            .unwrap();

        create_edit_tool()
            .execute(
                "edit-5",
                serde_json::json!({ "path": "edit.txt", "edits": [{ "oldText": "two", "newText": "TWO" }] }),
                CancellationToken::new(),
                None,
                ExecutionToolContext::new(Arc::clone(&env)),
            )
            .await
            .unwrap();

        let content = env.read_text_file("edit.txt", None).await.unwrap();
        assert_eq!(content, "\u{FEFF}one\r\nTWO\r\n");
    }

    #[tokio::test]
    async fn prepare_arguments_unwraps_json_string_edits_and_legacy_fields() {
        // `prepareEditArguments` (edit.ts:48-64).
        let tool: &dyn AgentHarnessTool<ExecutionToolContext> = &create_edit_tool();
        let prepared = tool.prepare_arguments(serde_json::json!({
            "path": "x.txt",
            "edits": r#"[{"oldText":"a","newText":"b"}]"#,
            "oldText": "legacy",
            "newText": "merged"
        }));
        assert_eq!(
            prepared["edits"],
            serde_json::json!([
                { "oldText": "a", "newText": "b" },
                { "oldText": "legacy", "newText": "merged" }
            ])
        );
        assert!(prepared.get("oldText").is_none());
        assert!(prepared.get("newText").is_none());
        assert_eq!(prepared["path"], Value::String("x.txt".into()));
    }

    #[test]
    fn tool_metadata() {
        let tool: &dyn AgentHarnessTool<ExecutionToolContext> = &create_edit_tool();
        assert_eq!(tool.name(), "edit");
        assert!(tool.description().contains("exact text replacement"));
    }
}
