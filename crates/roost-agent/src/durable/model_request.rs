//! Builds ordered Roost requests from durable model context.
//! Conversation and compaction requests reuse their configured model chains;
//! effective systems and tools are folded with Pi's transcript helpers.

use roost_llm::{ChatRequest, ToolSpec};
use rpi_ai::types::{
    Model, ModelThinkingLevel, RequestOriginKind, StreamOptions, TranscriptContext,
};

use crate::error::AgentError;
use crate::runtime::AgentRuntime;

use super::model_catalog::pi_model;
use crate::durable::message::from_durable_message;

pub(super) async fn requests_from_context(
    runtime: &AgentRuntime,
    conversation_id: &str,
    model: &Model,
    context: &TranscriptContext,
    options: &StreamOptions,
) -> Result<Vec<ChatRequest>, AgentError> {
    let record = runtime.record(conversation_id).await?;
    let compaction = options
        .origin
        .as_ref()
        .is_some_and(|origin| origin.kind == RequestOriginKind::Compaction);
    let chain = if compaction {
        crate::turn::side_task_models(runtime, &record)
            .await?
            .into_iter()
            .map(|info| (pi_model(&info), None))
            .collect::<Vec<_>>()
    } else {
        crate::turn::conversation_chain(runtime, &record)
            .await?
            .into_iter()
            .map(|candidate| (pi_model(&candidate.info), candidate.thinking))
            .collect::<Vec<_>>()
    };
    let selected_thinking = chain
        .iter()
        .find(|(candidate, _)| candidate.provider == model.provider && candidate.id == model.id)
        .and_then(|(_, thinking)| thinking.clone());
    let mut candidates = vec![(model.clone(), selected_thinking)];
    for (candidate_model, thinking) in chain {
        if !candidates.iter().any(|(existing, _)| {
            existing.provider == candidate_model.provider && existing.id == candidate_model.id
        }) {
            candidates.push((candidate_model, thinking));
        }
    }
    let entries = runtime.inner.store.entries(conversation_id).await?;
    let entries = crate::journal::effective_entries(&entries);
    let mut requests = Vec::with_capacity(candidates.len());
    for (index, (candidate_model, configured_thinking)) in candidates.iter().enumerate() {
        let mut memory = crate::turn::RunMemory::default();
        let info = runtime
            .inner
            .llm
            .catalog()
            .get(&candidate_model.provider, &candidate_model.id)
            .cloned()
            .ok_or_else(|| {
                AgentError::FailedPrecondition(format!(
                    "model {}/{} is absent from the Roost LLM catalog",
                    candidate_model.provider, candidate_model.id
                ))
            })?;
        let resolved_thinking = if index == 0 {
            if let Some(reasoning) = options.reasoning {
                Some(thinking_level(reasoning))
            } else if compaction && configured_thinking.is_none() {
                None
            } else {
                Some(
                    crate::turn::resolve_thinking(
                        runtime,
                        &record,
                        &crate::roles::ResolvedModel {
                            info,
                            thinking: configured_thinking.clone(),
                        },
                        &entries,
                        &mut memory,
                    )
                    .await,
                )
            }
        } else if compaction && configured_thinking.is_none() {
            None
        } else {
            Some(
                crate::turn::resolve_thinking(
                    runtime,
                    &record,
                    &crate::roles::ResolvedModel {
                        info,
                        thinking: configured_thinking.clone(),
                    },
                    &entries,
                    &mut memory,
                )
                .await,
            )
        };
        requests.push(request_from_context(
            runtime,
            conversation_id,
            candidate_model,
            context,
            options,
            resolved_thinking,
        )?);
    }
    Ok(requests)
}

fn request_from_context(
    runtime: &AgentRuntime,
    conversation_id: &str,
    model: &Model,
    context: &TranscriptContext,
    options: &StreamOptions,
    thinking: Option<&str>,
) -> Result<ChatRequest, AgentError> {
    let info = runtime
        .inner
        .llm
        .catalog()
        .get(&model.provider, &model.id)
        .cloned()
        .ok_or_else(|| {
            AgentError::FailedPrecondition(format!(
                "model {}/{} is absent from the Roost LLM catalog",
                model.provider, model.id
            ))
        })?;
    let systems = rpi_ai::utils::transcript::get_current_system_prompt(&context.messages);
    let current_tools = rpi_ai::utils::transcript::get_current_tools(&context.messages);
    if current_tools
        .iter()
        .any(|tool| tool.constrained_sampling.is_some())
    {
        return Err(AgentError::InvalidArgument(
            "constrained tool sampling is not representable by roost_llm ChatRequest".to_owned(),
        ));
    }
    let tools = current_tools
        .into_iter()
        .map(|tool| ToolSpec {
            name: tool.name,
            description: tool.description,
            parameters: tool.parameters,
        })
        .collect();
    let mut messages = context
        .messages
        .iter()
        .filter(|message| !matches!(message, rpi_ai::types::Message::System(_)))
        .map(|message| from_durable_message(message, info.images))
        .collect::<Result<Vec<_>, _>>()?;
    if context
        .messages
        .iter()
        .rev()
        .find(|message| !matches!(message, rpi_ai::types::Message::System(_)))
        .is_some_and(|message| {
            matches!(message, rpi_ai::types::Message::Assistant(assistant)
            if assistant.stop_reason == rpi_ai::types::StopReason::Pending)
        })
    {
        messages.push(roost_llm::Message::user_text(
            crate::prompts::RESUME_INTERRUPTED,
        ));
    }
    if options.api_key.is_some()
        || options.temperature.is_some()
        || options.sampling_params.is_some()
        || options.transport.is_some()
        || options.websocket_connect_timeout_ms.is_some()
        || options.metadata.is_some()
        || options.timeout_ms.is_some()
        || options.max_retries.is_some()
        || options.max_retry_delay_ms.is_some()
        || options.headers.is_some()
        || options.fetch.is_some()
        || options.env.is_some()
        || options.on_payload.is_some()
        || options.on_response.is_some()
    {
        return Err(AgentError::InvalidArgument(
            "Pi request options are not representable by roost_llm ChatRequest".to_owned(),
        ));
    }
    Ok(ChatRequest {
        model: info,
        system: if systems.is_empty() {
            Vec::new()
        } else {
            vec![systems]
        },
        messages,
        tools,
        thinking: thinking.unwrap_or("off").to_owned(),
        cache_retention: options.cache_retention.map(|retention| match retention {
            rpi_ai::types::CacheRetention::None => roost_llm::CacheRetention::None,
            rpi_ai::types::CacheRetention::Short => roost_llm::CacheRetention::Short,
            rpi_ai::types::CacheRetention::Long => roost_llm::CacheRetention::Long,
        }),
        session_id: options
            .session_id
            .as_deref()
            .unwrap_or(conversation_id)
            .to_owned(),
        max_tokens: Some(u64::from(options.max_tokens.unwrap_or(model.max_tokens))),
    })
}

fn thinking_level(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::Xhigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

pub(super) fn pi_thinking_level(level: &str) -> Option<ModelThinkingLevel> {
    match level {
        "off" => Some(ModelThinkingLevel::Off),
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::Xhigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}
