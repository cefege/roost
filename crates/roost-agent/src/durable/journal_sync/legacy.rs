//! Restores provider metadata carried by pre-kernel native assistant entries.
//! Journal synchronization calls this before hashing imported model messages;
//! incomplete progress stays context rather than assistant prefill.

use crate::records::Entry;
use rpi_ai::types::Message;

pub(super) fn enrich_legacy_message(message: &mut Message, source: &Entry) {
    let (
        Message::Assistant(message),
        Entry::Assistant {
            model,
            usage,
            metadata,
            ..
        },
    ) = (message, source)
    else {
        return;
    };
    if let Some(model) = model {
        message.provider.clone_from(&model.provider);
        message.model.clone_from(&model.model_id);
    }
    message.usage.input = usage.input;
    message.usage.output = usage.output;
    message.usage.cache_read = usage.cache_read;
    message.usage.cache_write = usage.cache_write;
    message.usage.total_tokens = usage
        .input
        .saturating_add(usage.output)
        .saturating_add(usage.cache_read)
        .saturating_add(usage.cache_write);
    if metadata
        .as_ref()
        .is_some_and(|metadata| !metadata.completed)
    {
        // Imported progress is context, not a provider-specific assistant prefill.
        message.stop_reason = rpi_ai::types::StopReason::Pending;
        if let Some(roost_llm::StopReason::Error(error)) = metadata
            .as_ref()
            .and_then(|metadata| metadata.stop_reason.as_ref())
        {
            message.error_message = Some(error.clone());
        }
        return;
    }
    message.stop_reason = match metadata
        .as_ref()
        .and_then(|metadata| metadata.stop_reason.as_ref())
    {
        Some(roost_llm::StopReason::EndTurn) => rpi_ai::types::StopReason::Stop,
        Some(roost_llm::StopReason::ToolUse) => rpi_ai::types::StopReason::ToolUse,
        Some(roost_llm::StopReason::MaxTokens) => rpi_ai::types::StopReason::Length,
        Some(roost_llm::StopReason::Error(error)) => {
            message.error_message = Some(error.clone());
            rpi_ai::types::StopReason::Error
        }
        None if message
            .content
            .iter()
            .any(|content| matches!(content, rpi_ai::types::AssistantContent::ToolCall(_))) =>
        {
            rpi_ai::types::StopReason::ToolUse
        }
        None => rpi_ai::types::StopReason::Stop,
    };
}
