//! Converts Roost model output into Pi assistant messages and terminal events.
//! The provider observer uses the same metadata mapping for partials and errors.

use roost_llm::{StopReason, Usage as LlmUsage};
use rpi_ai::types::{AssistantContent, AssistantMessage, Model, ModelThinkingLevel};

pub(super) fn assistant_message(
    model: &Model,
    content: Vec<AssistantContent>,
    usage: LlmUsage,
    stop: StopReason,
) -> AssistantMessage {
    let stop_reason = pi_stop_reason(&stop);
    let mut usage = pi_usage(usage);
    rpi_ai::utils::cost::calculate_cost(model, &mut usage);
    AssistantMessage {
        role: Default::default(),
        content,
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: Some(ModelThinkingLevel::Off),
        diagnostics: None,
        usage,
        stop_reason,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
        duration_ms: None,
    }
}

pub(super) fn pi_usage(usage: LlmUsage) -> rpi_ai::types::Usage {
    rpi_ai::types::Usage {
        input: usage.input,
        output: usage.output,
        cache_read: usage.cache_read,
        cache_write: usage.cache_write,
        cache_write1h: None,
        reasoning: None,
        total_tokens: usage
            .input
            .saturating_add(usage.output)
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write),
        cost: Default::default(),
    }
}

fn pi_stop_reason(reason: &StopReason) -> rpi_ai::types::StopReason {
    match reason {
        StopReason::EndTurn => rpi_ai::types::StopReason::Stop,
        StopReason::ToolUse => rpi_ai::types::StopReason::ToolUse,
        StopReason::MaxTokens => rpi_ai::types::StopReason::Length,
        StopReason::Error(_) => rpi_ai::types::StopReason::Error,
    }
}

pub(super) fn error_message(model: &Model, message: String) -> AssistantMessage {
    let mut assistant = assistant_message(
        model,
        Vec::new(),
        LlmUsage::default(),
        StopReason::Error(message.clone()),
    );
    assistant.error_message = Some(message);
    assistant
}
