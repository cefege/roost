//! Native journal rows for terminal Durable nested tool calls.
//!
//! Nested calls stay outside Durable model history; these side rows let Roost
//! retain their tool cards without inventing assistant messages or tool calls.

use std::collections::HashMap;

use rpi_ai::types::ToolCall;
use rpi_durable::TaskId;
use rpi_durable::chord::context::Context;
use rpi_durable::harness::{
    HarnessImpl,
    types::{DiagnosticSeverity, NestedToolExecutionResult, ToolDiagnostic},
};
use rpi_durable::types::{StorageWrite, TaskOutcome, TaskRecord, TaskState};
use serde_json::Value;

use crate::error::AgentError;
use crate::records::Entry;

/// Project terminal nested `pi.tool` task writes as native, model-excluded rows.
pub(super) fn nested_entries(writes: &[StorageWrite]) -> Result<Vec<Entry>, AgentError> {
    writes
        .iter()
        .filter_map(|write| match write {
            StorageWrite::Task { value }
                if value.kind == rpi_durable::harness::TOOL_TASK_NAME
                    && value.input.get("nested").is_some()
                    && matches!(&value.state, TaskState::Terminal { .. }) =>
            {
                Some(project_nested_task(value))
            }
            _ => None,
        })
        .collect()
}
pub(super) async fn terminal_item(
    harness: &HarnessImpl,
    task_id: TaskId,
    context: &Context,
) -> Result<Option<roost_protocol::wire::agent_chat::TranscriptItem>, AgentError> {
    let task = harness
        .session()
        .storage()
        .task(task_id, context)
        .await
        .map_err(|error| AgentError::Store(error.to_string()))?;
    let Some(task) = task else {
        return Ok(None);
    };
    let writes = [StorageWrite::Task { value: task }];
    let rows = nested_entries(&writes)?;
    Ok(rows
        .into_iter()
        .next()
        .and_then(|entry| crate::projection::entry_item(0, &entry, &HashMap::new())))
}

fn project_nested_task(task: &TaskRecord) -> Result<Entry, AgentError> {
    let nested = task
        .input
        .get("nested")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed(task, "nested call input"))?;
    let call_value = nested
        .get("call")
        .cloned()
        .ok_or_else(|| malformed(task, "nested tool call"))?;
    let call: ToolCall =
        serde_json::from_value(call_value).map_err(|error| AgentError::Store(error.to_string()))?;
    let parent_call_id = nested
        .get("parentCallId")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(task, "nested parent call id"))?;
    let TaskState::Terminal { outcome } = &task.state else {
        return Err(malformed(task, "terminal state"));
    };
    let receipt = outcome_result(outcome)
        .cloned()
        .map(serde_json::from_value::<NestedToolExecutionResult>)
        .transpose()
        .map_err(|error| AgentError::Store(error.to_string()))?;
    let (text, is_error, details_json) = match receipt {
        Some(receipt) => receipt_entry(&call, parent_call_id, receipt, outcome),
        None => failed_entry(&call, parent_call_id, outcome),
    };
    Ok(Entry::ToolResult {
        call_id: call.id,
        tool_name: call.name,
        text,
        is_error,
        details_json,
        content: None,
        provider_data: None,
    })
}

fn outcome_result(outcome: &TaskOutcome) -> Option<&Value> {
    match outcome {
        TaskOutcome::Completed { result } => Some(result),
        TaskOutcome::Failed { result, .. } | TaskOutcome::Aborted { result, .. } => result.as_ref(),
        TaskOutcome::Orphaned { .. } | TaskOutcome::Faulted { .. } => None,
    }
}

fn receipt_entry(
    call: &ToolCall,
    parent_call_id: &str,
    receipt: NestedToolExecutionResult,
    outcome: &TaskOutcome,
) -> (String, bool, String) {
    let mut text = structured_text(receipt.structured_output.as_ref());
    let is_error = receipt.is_error || !matches!(outcome, TaskOutcome::Completed { .. });
    append_diagnostics(&mut text, &receipt.diagnostics);
    let details = serde_json::json!({
        "arguments": call.arguments,
        "parentCallId": parent_call_id,
        "structuredOutput": receipt.structured_output,
        "details": receipt.details,
        "diagnostics": receipt.diagnostics,
        "usage": receipt.usage,
        "durationMs": receipt.duration_ms,
    });
    (text, is_error, details.to_string())
}

fn failed_entry(
    call: &ToolCall,
    parent_call_id: &str,
    outcome: &TaskOutcome,
) -> (String, bool, String) {
    let (status, text) = match outcome {
        TaskOutcome::Failed { error, .. } | TaskOutcome::Faulted { error } => {
            ("failed", error.message.clone())
        }
        TaskOutcome::Aborted { reason, .. } => (
            "aborted",
            reason
                .clone()
                .unwrap_or_else(|| "Nested tool was aborted".to_owned()),
        ),
        TaskOutcome::Orphaned { reason } => ("orphaned", reason.clone()),
        TaskOutcome::Completed { .. } => ("completed", String::new()),
    };
    let details = serde_json::json!({
        "arguments": call.arguments,
        "parentCallId": parent_call_id,
        "terminalOutcome": status,
        "outcomeUnknown": true,
    });
    (text, true, details.to_string())
}

fn structured_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(output) => output.to_string(),
    }
}

fn append_diagnostics(text: &mut String, diagnostics: &[ToolDiagnostic]) {
    if diagnostics.is_empty() {
        return;
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(
        &diagnostics
            .iter()
            .map(|diagnostic| {
                let severity = match diagnostic.severity {
                    DiagnosticSeverity::Info => "info",
                    DiagnosticSeverity::Warn => "warn",
                    DiagnosticSeverity::Error => "error",
                };
                format!("[{severity}] {}", diagnostic.message)
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

fn malformed(task: &TaskRecord, expected: &str) -> AgentError {
    AgentError::Store(format!("Nested Durable task {} lacks {expected}", task.id))
}
