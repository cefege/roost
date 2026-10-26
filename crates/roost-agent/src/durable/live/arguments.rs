//! Root tool-call arguments for durable live-slot presentation.
//!
//! Each call's args are loaded from its task's referenced assistant entry once;
//! progress frames reuse the cached JSON rather than rescanning conversation history.

use std::collections::HashMap;
use std::sync::Arc;

use rpi_ai::types::{AssistantContent, Message};
use rpi_durable::chord::context::Context;
use rpi_durable::chord::json::JsonObject;
use rpi_durable::harness::HarnessImpl;
use rpi_durable::{EntryId, TaskId};
use tokio::sync::Mutex;

use crate::error::AgentError;

pub(super) type RootArguments = Arc<Mutex<HashMap<String, String>>>;

pub(super) async fn root_args_json(
    call_id: &str,
    task_id: Option<TaskId>,
    harness: &HarnessImpl,
    cache: &RootArguments,
    context: &Context,
) -> String {
    if let Some(args_json) = cache.lock().await.get(call_id).cloned() {
        return args_json;
    }
    let Some(task_id) = task_id else {
        return "{}".to_owned();
    };
    let args_json = match read_root_call_args(task_id, call_id, harness, context).await {
        Ok(Some(arguments)) => {
            serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_owned())
        }
        Ok(None) => "{}".to_owned(),
        Err(error) => {
            tracing::warn!(call_id, %error, "could not read Durable tool call arguments");
            "{}".to_owned()
        }
    };
    cache
        .lock()
        .await
        .entry(call_id.to_owned())
        .or_insert_with(|| args_json.clone())
        .clone()
}

async fn read_root_call_args(
    task_id: TaskId,
    call_id: &str,
    harness: &HarnessImpl,
    context: &Context,
) -> Result<Option<JsonObject>, AgentError> {
    let task = harness
        .session()
        .storage()
        .task(task_id, context)
        .await
        .map_err(|error| AgentError::Store(error.to_string()))?;
    let Some(task) = task else {
        return Ok(None);
    };
    let Some(assistant_id) = task.input.get("assistant").cloned() else {
        return Ok(None);
    };
    let assistant_id: EntryId = serde_json::from_value(assistant_id)
        .map_err(|error| AgentError::Store(error.to_string()))?;
    let assistant_entry = harness
        .session()
        .storage()
        .entry(assistant_id, context)
        .await
        .map_err(|error| AgentError::Store(error.to_string()))?;
    let Some(assistant_entry) = assistant_entry else {
        return Ok(None);
    };
    Ok(assistant_entry
        .entry
        .model
        .unwrap_or_default()
        .into_iter()
        .find_map(|message| match message {
            Message::Assistant(assistant) => {
                assistant
                    .content
                    .into_iter()
                    .find_map(|content| match content {
                        AssistantContent::ToolCall(call) if call.id == call_id => {
                            Some(call.arguments)
                        }
                        _ => None,
                    })
            }
            _ => None,
        }))
}
