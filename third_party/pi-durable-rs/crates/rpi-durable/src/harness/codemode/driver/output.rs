//! Adapt portable sandbox results to durable tool output and storage.
//!
//! Typed images remain image blocks, and persistent store writes are committed
//! only with successful script completion.

use std::collections::BTreeMap;
use std::sync::Arc;

use rpi_codemode::{
    CodemodeCall, CodemodeCallStatus, CodemodeErrorKind, CodemodeOutputItem, CodemodeResult,
};
use serde_json::{Value, json};

use crate::chord::context::Context;
use crate::chord::json::JsonValue;
use crate::harness::codemode::{MAX_OUTPUT_BYTES, apply_store_writes, output, state};
use crate::harness::types::{ToolExecutionApi, ToolExecutionResult};
use crate::ids::{ConversationId, TaskId};

pub(super) async fn complete(
    result: CodemodeResult,
    api: Arc<dyn ToolExecutionApi>,
    task_id: TaskId,
    conversation_id: ConversationId,
    started_at_ms: u64,
    max_output_tokens: u64,
    generated_images: usize,
    store: BTreeMap<String, JsonValue>,
    context: Context,
) -> Result<ToolExecutionResult, crate::harness::types::ToolExecuteError> {
    let (script_result, calls, store_writes, is_error) = adapt_result(result);
    let now = api.now().map_err(super::super::tool_error)?;
    let wall_ms = if now.is_finite() && now >= started_at_ms as f64 {
        (now - started_at_ms as f64) as u64
    } else {
        0
    };
    let format_calls = calls
        .iter()
        .map(|call| (call.name.clone(), call.status.clone()))
        .collect();
    let env = api.env();
    let output_api = Arc::clone(&api);
    let output_context = context.clone();
    let output_task_id = task_id;
    let output_dir = Arc::new(tokio::sync::Mutex::new(None::<String>));
    let write: output::OutputFile = Arc::new(move |name, content| {
        let env = env.clone();
        let output_dir = Arc::clone(&output_dir);
        let api = Arc::clone(&output_api);
        let context = output_context.clone();
        Box::pin(async move {
            let env = env.ok_or_else(|| "Execution environment is unavailable".to_string())?;
            let mut directory = output_dir.lock().await;
            if directory.is_none() {
                let path =
                    match state::load_output_directory(api.as_ref(), output_task_id, &context)
                        .await
                        .map_err(|error| error.to_string())?
                    {
                        Some(path) => path,
                        None => {
                            let created = env
                                .create_temp_dir(Some("codemode-"), &context)
                                .await
                                .map_err(|error| error.to_string())?;
                            state::persist_output_directory(
                                api.as_ref(),
                                output_task_id,
                                created,
                                &context,
                            )
                            .await
                            .map_err(|error| error.to_string())?
                        }
                    };
                *directory = Some(path);
            }
            let file_path = match directory.as_deref() {
                Some(path) => format!("{}/{}", path.trim_end_matches('/'), name),
                None => return Err("Code-mode output directory was not initialized".into()),
            };
            env.write_file(&file_path, content, &context)
                .await
                .map_err(|error| error.to_string())?;
            Ok(file_path)
        })
    });
    let formatted = output::format_result(
        &script_result,
        &output::FormatOptions {
            generated_images,
            wall_ms,
            max_output_tokens,
            calls: format_calls,
            max_bytes: MAX_OUTPUT_BYTES,
            max_lines: 100_000,
        },
        write,
    )
    .await;
    let stored = if is_error {
        None
    } else {
        Some(apply_store_writes(store, store_writes))
    };
    let call_details = calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            json!({
                "id": index + 1,
                "name": &call.name,
                "status": &call.status,
                "durationMs": call.duration_ms,
            })
        })
        .collect::<Vec<_>>();
    let mut details = json!({ "calls": call_details });
    if let Some(path) = formatted.full_output_path {
        details["fullOutputPath"] = json!(path);
    }
    let final_result = ToolExecutionResult {
        content: Some(formatted.content),
        details: Some(details),
        is_error: Some(is_error),
        ..ToolExecutionResult::default()
    };
    state::persist_completion(
        api.as_ref(),
        task_id,
        conversation_id,
        final_result,
        stored,
        context,
    )
    .await
}

fn adapt_result(
    result: CodemodeResult,
) -> (
    output::ScriptResult,
    Vec<CallSummary>,
    Vec<(String, Option<Value>)>,
    bool,
) {
    match result {
        CodemodeResult::Ok {
            value,
            output: items,
            calls,
            store_writes,
        } => (
            output::ScriptResult {
                value,
                output: items.into_iter().map(adapt_output).collect(),
                error: None,
            },
            adapt_calls(calls),
            store_writes
                .set
                .into_iter()
                .map(|(key, value)| (key, Some(value)))
                .chain(store_writes.delete.into_iter().map(|key| (key, None)))
                .collect(),
            false,
        ),
        CodemodeResult::Err {
            error,
            output: items,
            calls,
        } => (
            output::ScriptResult {
                value: None,
                output: items.into_iter().map(adapt_output).collect(),
                error: Some(output::ScriptError {
                    kind: error_kind(error.kind).to_string(),
                    name: error.name,
                    message: error.message,
                    stack: error.stack,
                }),
            },
            adapt_calls(calls),
            Vec::new(),
            true,
        ),
    }
}

fn adapt_output(item: CodemodeOutputItem) -> output::OutputItem {
    match item {
        CodemodeOutputItem::Text { text, console } => output::OutputItem::Text { text, console },
        CodemodeOutputItem::Image { data, mime_type } => {
            output::OutputItem::Image { data, mime_type }
        }
    }
}

#[derive(Debug, Clone)]
struct CallSummary {
    name: String,
    status: String,
    duration_ms: f64,
}

fn adapt_calls(calls: Vec<CodemodeCall>) -> Vec<CallSummary> {
    calls
        .into_iter()
        .map(|call| CallSummary {
            name: call.name,
            status: call_status(call.status).to_string(),
            duration_ms: call.duration_ms,
        })
        .collect()
}

fn call_status(status: CodemodeCallStatus) -> &'static str {
    match status {
        CodemodeCallStatus::Ok => "ok",
        CodemodeCallStatus::Error => "error",
        CodemodeCallStatus::Cancelled => "cancelled",
    }
}

fn error_kind(kind: CodemodeErrorKind) -> &'static str {
    match kind {
        CodemodeErrorKind::Script => "script",
        CodemodeErrorKind::Timeout => "timeout",
        CodemodeErrorKind::Aborted => "aborted",
        CodemodeErrorKind::Sandbox => "sandbox",
    }
}
