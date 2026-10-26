//! Live Roost model and thinking choices applied before a Durable request pin.
//! Host settings may change between tool rounds; a request already checkpointed
//! keeps its model/options on recovery. Auto-thinking decisions are task memos.

use rpi_ai::types::ModelThinkingLevel;
use rpi_durable::Context;
use rpi_durable::harness::types::{Agent, HookApi, ModelRef};
use rpi_durable::session::{SessionError, SessionResult};

use super::state::ProjectionState;
use crate::runtime::AgentRuntime;

pub(super) async fn prepare(
    runtime: &AgentRuntime,
    conversation_id: &str,
    projection: &ProjectionState,
    mut agent: Agent,
    api: &dyn HookApi,
    context: &Context,
) -> SessionResult<Option<Agent>> {
    let record = runtime
        .record(conversation_id)
        .await
        .map_err(session_error)?;
    let model = crate::turn::conversation_model(runtime, &record)
        .await
        .map_err(session_error)?;
    let requested = model.thinking.as_deref().unwrap_or(&record.thinking_level);
    let thinking = if requested == "auto" {
        let entries = runtime
            .inner
            .store
            .entries(conversation_id)
            .await
            .map_err(session_error)?;
        let checkpoint = crate::journal::current_checkpoint(&entries);
        let cached = projection
            .auto_levels
            .lock()
            .map_err(|_| SessionError::message("Native thinking policy lock poisoned"))?
            .get(&checkpoint)
            .copied();
        let memo = format!("roost.thinking.{checkpoint}");
        let level: ModelThinkingLevel = match api.memo(&memo, context).await? {
            Some(value) => serde_json::from_value(value)
                .map_err(|error| SessionError::message(error.to_string()))?,
            None => {
                let level = if let Some(cached) = cached {
                    cached
                } else {
                    let text = crate::turn::latest_user_text(&entries);
                    let level = crate::auto_thinking::classify(
                        runtime,
                        conversation_id,
                        &model.info,
                        &text,
                    )
                    .await;
                    parse_level(model.info.clamp_thinking_level(level))?
                };
                let value = serde_json::to_value(level)
                    .map_err(|error| SessionError::message(error.to_string()))?;
                serde_json::from_value(api.memo_set(&memo, value, context).await?)
                    .map_err(|error| SessionError::message(error.to_string()))?
            }
        };
        projection
            .auto_levels
            .lock()
            .map_err(|_| SessionError::message("Native thinking policy lock poisoned"))?
            .insert(checkpoint, level);
        level
    } else {
        parse_level(model.info.clamp_thinking_level(requested))?
    };
    let thinking = parse_level(model.info.clamp_thinking_level(thinking.as_str()))?;
    agent.model = Some(ModelRef {
        provider: model.info.provider,
        model_id: model.info.id,
    });
    agent.thinking_level = thinking;
    Ok(Some(agent))
}

pub(super) fn parse_level(level: &str) -> SessionResult<ModelThinkingLevel> {
    serde_json::from_value(serde_json::json!(level))
        .map_err(|error| SessionError::message(error.to_string()))
}

fn session_error(error: crate::error::AgentError) -> SessionError {
    match error {
        crate::error::AgentError::Cancelled => SessionError::Aborted,
        other => SessionError::message(other.to_string()),
    }
}
