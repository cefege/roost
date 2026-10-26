//! Converts native provider-neutral messages into Pi's typed history.
//! The parent module owns metadata envelopes and the reverse conversion;
//! callers retain rich content and reject unsupported provider fields.

use crate::error::AgentError;
use roost_llm::{AssistantBlock, Message as LlmMessage, UserContent};
use rpi_ai::types::{
    AssistantContent, ImageContent, Message, TextContent, ThinkingContent, ToolCall,
    ToolResultContent, UserContent as DurableUserContent, UserContentBlock,
};

use super::{
    empty_assistant, json_error, provider_bool, provider_string, reject_unknown, string_field,
    value_field,
};

/// Convert a Roost message into Pi's typed durable message representation.
pub(crate) fn to_durable_message(message: &LlmMessage) -> Result<Message, AgentError> {
    match message {
        LlmMessage::User { content } => Ok(Message::User(rpi_ai::types::UserMessage {
            role: Default::default(),
            content: DurableUserContent::Blocks(
                content
                    .iter()
                    .map(|block| match block {
                        UserContent::Text { text } => UserContentBlock::Text(TextContent {
                            text: text.clone(),
                            text_signature: None,
                        }),
                        UserContent::Image { mime, base64 } => {
                            UserContentBlock::Image(ImageContent {
                                data: base64.clone(),
                                mime_type: mime.clone(),
                            })
                        }
                    })
                    .collect(),
            ),
            timestamp: 0,
        })),
        LlmMessage::Assistant {
            blocks,
            provider_data,
        } => {
            let content = blocks
                .iter()
                .map(|block| match block {
                    AssistantBlock::Text {
                        text,
                        provider_data,
                    } => {
                        let signature = provider_string(provider_data.as_ref(), "textSignature")?;
                        reject_unknown(provider_data.as_ref(), &["textSignature"])?;
                        Ok(AssistantContent::Text(TextContent {
                            text: text.clone(),
                            text_signature: signature,
                        }))
                    }
                    AssistantBlock::Thinking {
                        text,
                        provider_data,
                    } => {
                        let redacted = provider_bool(provider_data.as_ref(), "redacted")?;
                        let thinking_signature = provider_data
                            .as_ref()
                            .map(|data| {
                                serde_json::to_string(data)
                                    .map(|serialized| {
                                        format!("roost-provider-data:v1:{serialized}")
                                    })
                                    .map_err(json_error)
                            })
                            .transpose()?;
                        Ok(AssistantContent::Thinking(ThinkingContent {
                            thinking: text.clone(),
                            thinking_signature,
                            redacted,
                        }))
                    }
                    AssistantBlock::ToolCall {
                        call_id,
                        name,
                        args_json,
                        provider_data,
                    } => {
                        let arguments = serde_json::from_str::<serde_json::Value>(args_json)
                            .map_err(|error| {
                                AgentError::InvalidArgument(format!(
                                    "tool call {call_id} has invalid JSON arguments: {error}"
                                ))
                            })?
                            .as_object()
                            .cloned()
                            .ok_or_else(|| {
                                AgentError::InvalidArgument(format!(
                                    "tool call {call_id} arguments must be a JSON object"
                                ))
                            })?;
                        let thought_signature =
                            provider_string(provider_data.as_ref(), "thoughtSignature")?;
                        let namespace = provider_string(provider_data.as_ref(), "namespace")?;
                        reject_unknown(provider_data.as_ref(), &["thoughtSignature", "namespace"])?;
                        Ok(AssistantContent::ToolCall(ToolCall {
                            id: call_id.clone(),
                            name: name.clone(),
                            arguments,
                            thought_signature,
                            namespace,
                        }))
                    }
                })
                .collect::<Result<Vec<_>, AgentError>>()?;
            let mut assistant = empty_assistant(content);
            if let Some(fields) = provider_data.as_ref() {
                reject_unknown(
                    Some(fields),
                    &[
                        "api",
                        "provider",
                        "model",
                        "responseModel",
                        "responseId",
                        "providerThinkingLevel",
                        "thinkingLevel",
                        "diagnostics",
                        "usage",
                        "stopReason",
                        "deferred",
                        "errorMessage",
                        "rawStopReason",
                        "endTurn",
                        "timestamp",
                        "durationMs",
                    ],
                )?;
                assistant.api = value_field(fields, "api")?
                    .unwrap_or_else(|| rpi_ai::types::ApiKind(String::new()));
                assistant.provider = string_field(fields, "provider")?.unwrap_or_default();
                assistant.model = string_field(fields, "model")?.unwrap_or_default();
                assistant.response_model = string_field(fields, "responseModel")?;
                assistant.response_id = string_field(fields, "responseId")?;
                assistant.provider_thinking_level = string_field(fields, "providerThinkingLevel")?;
                assistant.thinking_level = value_field(fields, "thinkingLevel")?;
                assistant.diagnostics = value_field(fields, "diagnostics")?;
                assistant.usage = value_field(fields, "usage")?.unwrap_or_default();
                assistant.stop_reason = value_field(fields, "stopReason")?
                    .unwrap_or(rpi_ai::types::StopReason::Pending);
                assistant.deferred = value_field(fields, "deferred")?;
                assistant.error_message = string_field(fields, "errorMessage")?;
                assistant.raw_stop_reason = string_field(fields, "rawStopReason")?;
                assistant.end_turn = value_field(fields, "endTurn")?;
                assistant.timestamp = value_field(fields, "timestamp")?.unwrap_or_default();
                assistant.duration_ms = value_field(fields, "durationMs")?;
            }
            Ok(Message::Assistant(assistant))
        }
        LlmMessage::ToolResult {
            call_id,
            tool_name,
            text,
            is_error,
            content,
            provider_data,
        } => {
            let durable_content = content
                .as_ref()
                .map(|blocks| {
                    blocks
                        .iter()
                        .map(|block| match block {
                            UserContent::Text { text } => ToolResultContent::Text(TextContent {
                                text: text.clone(),
                                text_signature: None,
                            }),
                            UserContent::Image { mime, base64 } => {
                                ToolResultContent::Image(ImageContent {
                                    data: base64.clone(),
                                    mime_type: mime.clone(),
                                })
                            }
                        })
                        .collect()
                })
                .unwrap_or_else(|| {
                    vec![ToolResultContent::Text(TextContent {
                        text: text.clone(),
                        text_signature: None,
                    })]
                });
            let fields = provider_data
                .as_ref()
                .and_then(serde_json::Value::as_object);
            reject_unknown(provider_data.as_ref(), &["details", "usage", "nestedCalls"])?;
            Ok(Message::ToolResult(rpi_ai::types::ToolResultMessage {
                role: Default::default(),
                tool_call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                content: durable_content,
                details: fields.and_then(|fields| fields.get("details").cloned()),
                usage: fields
                    .and_then(|fields| fields.get("usage").cloned())
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|error| AgentError::InvalidArgument(error.to_string()))?,
                nested_calls: fields
                    .and_then(|fields| fields.get("nestedCalls").cloned())
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|error| AgentError::InvalidArgument(error.to_string()))?,
                is_error: *is_error,
                timestamp: 0,
                duration_ms: None,
            }))
        }
    }
}
