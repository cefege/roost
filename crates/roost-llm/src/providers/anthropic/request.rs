//! Anthropic request bodies and replay-safe assistant blocks.
//! The streaming client supplies native requests and OAuth identity mode;
//! cache breakpoints and signed reasoning stay provider-local here.

use serde_json::{Value, json};

use crate::message::{AssistantBlock, CacheRetention, ChatRequest, Message};

pub(super) fn build_body(request: &ChatRequest, oauth: bool) -> Value {
    let cache_control = match request.cache_retention {
        Some(CacheRetention::None) => None,
        Some(CacheRetention::Long) => Some(json!({"type":"ephemeral","ttl":"1h"})),
        _ => Some(json!({"type":"ephemeral"})),
    };
    let mut system = Vec::new();
    if oauth {
        system.push(json!({"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude."}));
    }
    for text in &request.system {
        system.push(json!({"type":"text","text":text}));
    }
    if let (Some(last_system), Some(cache_control)) = (system.last_mut(), &cache_control) {
        last_system["cache_control"] = cache_control.clone();
    }
    let user_total = request
        .messages
        .iter()
        .filter(|message| matches!(message, Message::User { .. } | Message::ToolResult { .. }))
        .count();
    let mut user_idx = 0;
    let mut messages = Vec::new();
    for message in &request.messages {
        match message {
            Message::User { content } => {
                let mut blocks: Vec<Value> = content.iter().map(|part| match part {
                    crate::message::UserContent::Text { text } => json!({"type":"text","text":text}),
                    crate::message::UserContent::Image { mime, base64 } => json!({"type":"image","source":{"type":"base64","media_type":mime,"data":base64}}),
                }).collect();
                if user_idx + 2 >= user_total
                    && let (Some(last), Some(cache_control)) = (blocks.last_mut(), &cache_control) {
                    last["cache_control"] = cache_control.clone();
                }
                user_idx += 1;
                messages.push(json!({"role":"user","content":blocks}));
            }
            Message::Assistant { blocks, .. } => messages.push(json!({"role":"assistant","content":blocks.iter().map(anthropic_block).collect::<Vec<_>>()})),
            Message::ToolResult {
                call_id,
                text,
                is_error,
                content,
                ..
            } => {
                user_idx += 1;
                let result_content = content.as_ref().map(|content| {
                    Value::Array(content.iter().map(|part| match part {
                        crate::message::UserContent::Text { text } => json!({"type":"text","text":text}),
                        crate::message::UserContent::Image { mime, base64 } => json!({"type":"image","source":{"type":"base64","media_type":mime,"data":base64}}),
                    }).collect::<Vec<_>>())
                }).unwrap_or_else(|| json!(text));
                let mut result = json!({"type":"tool_result","tool_use_id":call_id,"content":result_content,"is_error":is_error});
                if user_idx + 1 >= user_total && let Some(cache_control) = &cache_control {
                    result["cache_control"] = cache_control.clone();
                }
                messages.push(json!({"role":"user","content":[result]}));
            }
        }
    }
    let mut body = json!({"model":request.model.id,"max_tokens":request.max_tokens.unwrap_or(request.model.max_tokens),"stream":true,"messages":messages,"system":system});
    if request.model.reasoning {
        let thinking = request.model.provider_thinking_value(&request.thinking);
        if request.model.compat_flag("forceAdaptiveThinking") {
            if let Some(effort) = thinking {
                body["thinking"] = json!({"type":"adaptive"});
                body["output_config"] = json!({"effort":effort});
            }
        } else if thinking.is_some() {
            body["thinking"] = json!({"type":"enabled","budget_tokens":(request.max_tokens.unwrap_or(request.model.max_tokens)/2).clamp(1024, 32000)});
        } else if request.thinking == "off"
            && request.model.thinking_level_map.get("off") != Some(&None)
        {
            body["thinking"] = json!({"type":"disabled"});
        }
    }
    let tools: Vec<Value> = request.tools.iter().enumerate().map(|(index, tool)| {
        let mut value = json!({"name":tool.name,"description":tool.description,"input_schema":tool.parameters});
        if index + 1 == request.tools.len() && let Some(cache_control) = &cache_control {
            value["cache_control"] = cache_control.clone();
        }
        value
    }).collect();
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    body
}

fn anthropic_block(block: &AssistantBlock) -> Value {
    match block {
        AssistantBlock::Text { text, .. } => json!({"type":"text","text":text}),
        AssistantBlock::Thinking {
            text,
            provider_data,
        } => match provider_data {
            Some(data) if data.get("redacted_thinking").is_some() => {
                json!({"type":"redacted_thinking","data":data["redacted_thinking"]})
            }
            // A dropped stream can persist thinking before its signature arrives.
            Some(data)
                if data
                    .get("signature")
                    .and_then(Value::as_str)
                    .is_some_and(|signature| !signature.is_empty()) =>
            {
                json!({"type":"thinking","thinking":text,"signature":data["signature"]})
            }
            _ => json!({"type":"text","text":text}),
        },
        AssistantBlock::ToolCall {
            call_id,
            name,
            args_json,
            ..
        } => {
            json!({"type":"tool_use","id":call_id,"name":name,"input":serde_json::from_str::<Value>(args_json).unwrap_or_else(|_| json!({}))})
        }
    }
}
