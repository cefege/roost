//! Loss-aware conversion between durable Pi history and Roost LLM messages.
//!
//! Durable persistence remains outside this adapter; these helpers only translate
//! provider context and reject fields the provider-neutral type cannot carry.

use crate::error::AgentError;
use roost_llm::{AssistantBlock, Message as LlmMessage, UserContent};
use rpi_ai::types::{
    AssistantContent, Message, ThinkingContent, ToolResultContent,
    UserContent as DurableUserContent, UserContentBlock,
};

mod outbound;
pub(super) use outbound::to_durable_message;

/// Convert a Pi transcript message to Roost's provider-neutral request form.
pub(super) fn from_durable_message(message: &Message) -> Result<LlmMessage, AgentError> {
    match message {
        Message::User(user) => {
            let content = match &user.content {
                DurableUserContent::Text(text) => vec![UserContent::Text { text: text.clone() }],
                DurableUserContent::Blocks(blocks) => blocks
                    .iter()
                    .map(|block| match block {
                        UserContentBlock::Text(text) => {
                            if text.text_signature.is_some() {
                                return Err(unsupported("signed user text"));
                            }
                            Ok(UserContent::Text {
                                text: text.text.clone(),
                            })
                        }
                        UserContentBlock::Image(image) => Ok(UserContent::Image {
                            mime: image.mime_type.clone(),
                            base64: image.data.clone(),
                        }),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            };
            Ok(LlmMessage::User { content })
        }
        Message::Assistant(assistant) => Ok(LlmMessage::Assistant {
            blocks: from_durable_blocks(&assistant.content)?,
            provider_data: Some(assistant_envelope(assistant)?),
        }),
        Message::ToolResult(result) => {
            let content = result
                .content
                .iter()
                .map(|block| match block {
                    ToolResultContent::Text(text) => {
                        if text.text_signature.is_some() {
                            return Err(unsupported("signed tool-result text"));
                        }
                        Ok(UserContent::Text {
                            text: text.text.clone(),
                        })
                    }
                    ToolResultContent::Image(image) => Ok(UserContent::Image {
                        mime: image.mime_type.clone(),
                        base64: image.data.clone(),
                    }),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut provider_data = serde_json::Map::new();
            if let Some(details) = &result.details {
                provider_data.insert("details".into(), details.clone());
            }
            if let Some(usage) = &result.usage {
                provider_data.insert(
                    "usage".into(),
                    serde_json::to_value(usage).map_err(json_error)?,
                );
            }
            if let Some(nested) = &result.nested_calls {
                provider_data.insert(
                    "nestedCalls".into(),
                    serde_json::to_value(nested).map_err(json_error)?,
                );
            }
            let text = content
                .iter()
                .filter_map(|block| match block {
                    UserContent::Text { text } => Some(text.as_str()),
                    UserContent::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(LlmMessage::ToolResult {
                call_id: result.tool_call_id.clone(),
                tool_name: result.tool_name.clone(),
                text,
                is_error: result.is_error,
                content: Some(content),
                provider_data: (!provider_data.is_empty()).then_some(provider_data.into()),
            })
        }
        Message::System(_) => Err(unsupported(
            "positional system messages (use effective-system folding)",
        )),
    }
}

/// Convert Pi response blocks, retaining signed/provider-specific metadata.
pub(super) fn from_durable_blocks(
    blocks: &[AssistantContent],
) -> Result<Vec<AssistantBlock>, AgentError> {
    blocks
        .iter()
        .map(|block| match block {
            AssistantContent::Text(text) => Ok(AssistantBlock::Text {
                text: text.text.clone(),
                provider_data: text
                    .text_signature
                    .as_ref()
                    .map(|signature| serde_json::json!({"textSignature": signature})),
            }),
            AssistantContent::Thinking(thinking) => Ok(AssistantBlock::Thinking {
                text: thinking.thinking.clone(),
                provider_data: thinking_provider_data(thinking)?,
            }),
            AssistantContent::ToolCall(call) => Ok(AssistantBlock::ToolCall {
                call_id: call.id.clone(),
                name: call.name.clone(),
                args_json: serde_json::to_string(&call.arguments).map_err(json_error)?,
                provider_data: (call.thought_signature.is_some() || call.namespace.is_some()).then(
                    || {
                        serde_json::json!({
                            "thoughtSignature": call.thought_signature, "namespace": call.namespace,
                        })
                    },
                ),
            }),
        })
        .collect()
}

fn assistant_envelope(
    assistant: &rpi_ai::types::AssistantMessage,
) -> Result<serde_json::Value, AgentError> {
    let mut fields = serde_json::Map::new();
    fields.insert(
        "api".into(),
        serde_json::to_value(&assistant.api).map_err(json_error)?,
    );
    fields.insert("provider".into(), assistant.provider.clone().into());
    fields.insert("model".into(), assistant.model.clone().into());
    for (key, value) in [
        (
            "responseModel",
            serde_json::to_value(&assistant.response_model).map_err(json_error)?,
        ),
        (
            "responseId",
            serde_json::to_value(&assistant.response_id).map_err(json_error)?,
        ),
        (
            "providerThinkingLevel",
            serde_json::to_value(&assistant.provider_thinking_level).map_err(json_error)?,
        ),
        (
            "thinkingLevel",
            serde_json::to_value(assistant.thinking_level).map_err(json_error)?,
        ),
        (
            "diagnostics",
            serde_json::to_value(&assistant.diagnostics).map_err(json_error)?,
        ),
        (
            "usage",
            serde_json::to_value(&assistant.usage).map_err(json_error)?,
        ),
        (
            "stopReason",
            serde_json::to_value(assistant.stop_reason).map_err(json_error)?,
        ),
        (
            "deferred",
            serde_json::to_value(&assistant.deferred).map_err(json_error)?,
        ),
        (
            "errorMessage",
            serde_json::to_value(&assistant.error_message).map_err(json_error)?,
        ),
        (
            "rawStopReason",
            serde_json::to_value(&assistant.raw_stop_reason).map_err(json_error)?,
        ),
        (
            "endTurn",
            serde_json::to_value(assistant.end_turn).map_err(json_error)?,
        ),
        (
            "timestamp",
            serde_json::to_value(assistant.timestamp).map_err(json_error)?,
        ),
        (
            "durationMs",
            serde_json::to_value(assistant.duration_ms).map_err(json_error)?,
        ),
    ] {
        if !value.is_null() {
            fields.insert(key.into(), value);
        }
    }
    Ok(fields.into())
}

fn value_field<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
    key: &str,
) -> Result<Option<T>, AgentError> {
    value
        .get(key)
        .filter(|value| !value.is_null())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(json_error)
}

fn string_field(value: &serde_json::Value, key: &str) -> Result<Option<String>, AgentError> {
    match value.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(unsupported("assistant envelope field type")),
    }
}

fn thinking_provider_data(
    thinking: &ThinkingContent,
) -> Result<Option<serde_json::Value>, AgentError> {
    let Some(signature) = &thinking.thinking_signature else {
        return Ok(thinking
            .redacted
            .map(|redacted| serde_json::json!({ "redacted": redacted })));
    };
    if let Some(serialized) = signature.strip_prefix("roost-provider-data:v1:") {
        return serde_json::from_str(serialized)
            .map(Some)
            .map_err(json_error);
    }
    let mut fields = serde_json::Map::new();
    fields.insert("thinkingSignature".to_owned(), signature.clone().into());
    if let Some(redacted) = thinking.redacted {
        fields.insert("redacted".to_owned(), redacted.into());
    }
    Ok(Some(fields.into()))
}

fn provider_string(
    value: Option<&serde_json::Value>,
    key: &str,
) -> Result<Option<String>, AgentError> {
    match value.and_then(|value| value.get(key)) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(unsupported("provider metadata type")),
    }
}

fn provider_bool(value: Option<&serde_json::Value>, key: &str) -> Result<Option<bool>, AgentError> {
    match value.and_then(|value| value.get(key)) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(unsupported("provider metadata type")),
    }
}

fn reject_unknown(value: Option<&serde_json::Value>, allowed: &[&str]) -> Result<(), AgentError> {
    if value
        .and_then(serde_json::Value::as_object)
        .is_some_and(|fields| fields.keys().any(|key| !allowed.contains(&key.as_str())))
        || value.is_some_and(|value| !value.is_object())
    {
        return Err(unsupported("unknown provider metadata"));
    }
    Ok(())
}

fn empty_assistant(content: Vec<AssistantContent>) -> rpi_ai::types::AssistantMessage {
    rpi_ai::types::AssistantMessage {
        role: Default::default(),
        content,
        api: rpi_ai::types::ApiKind(String::new()),
        provider: String::new(),
        model: String::new(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Default::default(),
        stop_reason: rpi_ai::types::StopReason::Pending,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
        duration_ms: None,
    }
}

fn json_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::InvalidArgument(format!("invalid provider message metadata: {error}"))
}

fn unsupported(field: &str) -> AgentError {
    AgentError::InvalidArgument(format!(
        "durable provider message contains {field} not representable by roost_llm"
    ))
}
