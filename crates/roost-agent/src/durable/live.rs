//! Durable `pi.live` state projected into existing Roost tool transcript items.
//!
//! The durable Harness owns live status and output; this watch only adapts its
//! serialized root and nested tool slots to Roost's chat event surface.

mod arguments;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use roost_protocol::wire::agent_chat::{ChatEvent, TranscriptItem};
use rpi_durable::chord::context::Context;
use rpi_durable::chord::json::{JsonObject, JsonValue};
use rpi_durable::harness::{HarnessImpl, LIVE_DOC, LiveState};
use rpi_durable::session::{CommittedWatch, DocOwner, WatchFuture, WatchListener};
use rpi_durable::{BACKGROUND_CONTEXT, ROOT_CONVERSATION_ID};
use tokio::sync::Mutex;

use super::state::ProjectionState;
use crate::error::AgentError;
use crate::runtime::AgentRuntime;

pub(super) struct LiveView {
    watch: CommittedWatch,
    publish: Arc<Mutex<()>>,
    closed: Arc<AtomicBool>,
}

impl LiveView {
    pub(super) async fn open(
        runtime: AgentRuntime,
        id: String,
        harness: Arc<HarnessImpl>,
        projection: Arc<ProjectionState>,
    ) -> Result<Self, AgentError> {
        let watch = harness
            .session()
            .watch_doc(
                &LIVE_DOC,
                DocOwner::Conversation(ROOT_CONVERSATION_ID),
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(session_error)?
            .ok_or_else(|| AgentError::Store("Durable live document is absent".into()))?;
        let initial = decode_live_state(watch.value())?
            .ok_or_else(|| AgentError::Store("Durable live document is retired".into()))?;
        let publish = Arc::new(Mutex::new(()));
        let closed = Arc::new(AtomicBool::new(false));
        let root_arguments = arguments::RootArguments::default();
        let nested_final = Arc::new(Mutex::new(HashMap::new()));
        {
            let _serial = publish.lock().await;
            publish_state(
                &runtime,
                &id,
                &harness,
                &projection,
                &root_arguments,
                &nested_final,
                initial,
                &BACKGROUND_CONTEXT,
            )
            .await?;
        }

        let listener_runtime = runtime;
        let listener_id = id;
        let listener_harness = Arc::clone(&harness);
        let listener_projection = Arc::clone(&projection);
        let listener_arguments = Arc::clone(&root_arguments);
        let listener_publish = Arc::clone(&publish);
        let listener_nested_final = Arc::clone(&nested_final);
        let listener_closed = Arc::clone(&closed);
        let listener: WatchListener = Arc::new(move |value, _operations, context| {
            let runtime = listener_runtime.clone();
            let id = listener_id.clone();
            let harness = Arc::clone(&listener_harness);
            let projection = Arc::clone(&listener_projection);
            let root_arguments = Arc::clone(&listener_arguments);
            let nested_final = Arc::clone(&listener_nested_final);
            let closed = Arc::clone(&listener_closed);
            let publish = Arc::clone(&listener_publish);
            Box::pin(async move {
                let Some(state) = decode_live_state(value).map_err(|error| error.to_string())?
                else {
                    return Ok(());
                };
                let _serial = publish.lock().await;
                if closed.load(Ordering::SeqCst) {
                    return Ok(());
                }
                publish_state(
                    &runtime,
                    &id,
                    &harness,
                    &projection,
                    &root_arguments,
                    &nested_final,
                    state,
                    &context,
                )
                .await
                .map_err(|error| error.to_string())
            }) as WatchFuture
        });
        if let Err(error) = watch.start(listener) {
            watch.stop().await;
            return Err(session_error(error));
        }
        Ok(Self {
            watch,
            publish,
            closed,
        })
    }

    pub(super) async fn close(&self) {
        self.watch.stop().await;
        let _serial = self.publish.lock().await;
        self.closed.store(true, Ordering::SeqCst);
    }
}

impl Drop for LiveView {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        self.watch.cancel();
    }
}

#[allow(clippy::too_many_arguments)]
async fn publish_state(
    runtime: &AgentRuntime,
    conversation_id: &str,
    harness: &Arc<HarnessImpl>,
    projection: &Arc<ProjectionState>,
    root_arguments: &arguments::RootArguments,
    nested_final: &Arc<Mutex<HashMap<String, TranscriptItem>>>,
    state: LiveState,
    context: &Context,
) -> Result<(), AgentError> {
    let mut events = Vec::new();
    let active_root_calls = state
        .tools
        .as_ref()
        .into_iter()
        .flatten()
        .map(|slot| slot.call_id.clone())
        .collect::<HashSet<_>>();
    if let Some(slots) = state.tools {
        for slot in slots {
            let args_json = arguments::root_args_json(
                &slot.call_id,
                slot.task_id,
                harness,
                root_arguments,
                context,
            )
            .await;
            if slot.status == rpi_durable::harness::live::ToolSlotStatus::Done {
                let Some(entry_id) = slot.entry else {
                    continue;
                };
                let Some(entry_record) = harness
                    .session()
                    .storage()
                    .entry(entry_id, context)
                    .await
                    .map_err(session_error)?
                else {
                    tracing::warn!(
                        conversation_id,
                        call_id = slot.call_id,
                        entry_id = entry_id.get(),
                        "Durable live tool slot references a missing result entry"
                    );
                    continue;
                };
                let entry = super::projection::project(&entry_record.entry, projection)?;
                let tool_calls =
                    HashMap::from([(slot.call_id.clone(), (slot.name.clone(), args_json))]);
                if let Some(item) = crate::projection::entry_item(0, &entry, &tool_calls) {
                    events.push(ChatEvent::Item { item });
                }
                continue;
            }
            let details_json = live_details_json(
                slot.details,
                slot.diagnostics,
                serde_json::json!({
                    "droppedBytes": slot.dropped_bytes,
                    "droppedLines": slot.dropped_lines,
                }),
            );
            events.push(ChatEvent::Item {
                item: TranscriptItem::Tool {
                    id: crate::projection::tool_item_id(&slot.call_id),
                    call_id: slot.call_id,
                    tool_name: slot.name,
                    args_json,
                    output: slot.output.unwrap_or_default(),
                    is_error: false,
                    running: true,
                    queued: slot.status == rpi_durable::harness::live::ToolSlotStatus::Pending,
                    children: child_conversations(&details_json),
                    details_json,
                },
            });
        }
    }
    root_arguments
        .lock()
        .await
        .retain(|call_id, _| active_root_calls.contains(call_id));

    let active_nested_calls = state
        .nested_tools
        .as_ref()
        .into_iter()
        .flatten()
        .map(|slot| slot.call_id.clone())
        .collect::<HashSet<_>>();
    if let Some(slots) = state.nested_tools {
        for slot in slots {
            if slot.status == rpi_durable::harness::live::ToolSlotStatus::Done {
                let cached = nested_final.lock().await.get(&slot.call_id).cloned();
                let final_item = if let Some(item) = cached {
                    Some(item)
                } else {
                    super::nested_projection::terminal_item(harness, slot.task_id, context).await?
                };
                if let Some(item) = final_item {
                    nested_final
                        .lock()
                        .await
                        .insert(slot.call_id.clone(), item.clone());
                    events.push(ChatEvent::Item { item });
                    continue;
                }
            }
            let arguments = JsonValue::Object(slot.arguments);
            let args_json = serde_json::to_string(&arguments)
                .map_err(|error| AgentError::Store(error.to_string()))?;
            let details_json = live_details_json(
                slot.details,
                slot.diagnostics,
                serde_json::json!({
                    "arguments": arguments,
                    "parentCallId": slot.parent_call_id,
                    "droppedBytes": slot.dropped_bytes,
                    "droppedLines": slot.dropped_lines,
                }),
            );
            events.push(ChatEvent::Item {
                item: TranscriptItem::Tool {
                    id: crate::projection::tool_item_id(&slot.call_id),
                    call_id: slot.call_id,
                    tool_name: slot.name,
                    args_json,
                    output: slot.output.unwrap_or_default(),
                    is_error: false,
                    running: slot.status == rpi_durable::harness::live::ToolSlotStatus::Running,
                    queued: slot.status == rpi_durable::harness::live::ToolSlotStatus::Pending,
                    details_json,
                    children: Vec::new(),
                },
            });
        }
    }
    nested_final
        .lock()
        .await
        .retain(|call_id, _| active_nested_calls.contains(call_id));
    if !events.is_empty() {
        runtime.emit(conversation_id, events).await;
    }
    Ok(())
}

fn decode_live_state(value: Option<JsonObject>) -> Result<Option<LiveState>, AgentError> {
    value
        .map(|value| {
            serde_json::from_value(JsonValue::Object(value))
                .map_err(|error| AgentError::Store(error.to_string()))
        })
        .transpose()
}

fn live_details_json(
    details: Option<JsonValue>,
    diagnostics: Option<Vec<rpi_durable::harness::ToolDiagnostic>>,
    metadata: JsonValue,
) -> String {
    let mut value = match metadata {
        JsonValue::Object(object) => object,
        _ => serde_json::Map::new(),
    };
    if let Some(details) = details {
        value.insert("details".to_owned(), details);
    }
    if let Some(diagnostics) = diagnostics {
        value.insert(
            "diagnostics".to_owned(),
            serde_json::to_value(diagnostics).unwrap_or(JsonValue::Null),
        );
    }
    JsonValue::Object(value).to_string()
}

fn child_conversations(details_json: &str) -> Vec<String> {
    serde_json::from_str::<JsonValue>(details_json)
        .ok()
        .and_then(|details| details.get("details").cloned())
        .and_then(|details| details.get("children").cloned())
        .and_then(|children| children.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|child| child.as_str().map(str::to_owned))
        .collect()
}

fn session_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::Store(error.to_string())
}
