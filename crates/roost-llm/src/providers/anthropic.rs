//! Ported from pi-ai 1.1.0 dist/api/anthropic-messages.js (MIT).
//! Builds Anthropic Messages requests, including cache breakpoints and OAuth
//! identity headers, then folds message SSE events into provider-neutral blocks.

use futures::StreamExt;
use reqwest::header::HeaderValue;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{HeaderObserver, ProviderStream, auth_headers, decode_json, post_sse, response_events};
use crate::{
    auth::ResolvedAuth,
    endpoints::Endpoints,
    message::{AssistantBlock, BlockKind, ChatRequest, Message, StopReason, StreamEvent, Usage},
};

pub(super) fn stream(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    request: ChatRequest,
    auth: ResolvedAuth,
    cancel: CancellationToken,
    observer: Option<HeaderObserver>,
) -> ProviderStream {
    let http = http.clone();
    let endpoints = endpoints.clone();
    Box::pin(async_stream::try_stream! {
    let base = endpoints.base(&request.model.provider, &request.model.base_url);
    let url = format!("{}/v1/messages", base.trim_end_matches('/'));
    let oauth = auth.is_oauth();
    let mut headers = auth_headers(&auth, "x-api-key", request.model.provider == "openrouter")?;
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    let mut betas: Vec<&str> = Vec::new();
    if oauth {
        betas.extend(["claude-code-20250219", "oauth-2025-04-20"]);
        headers.insert("user-agent", HeaderValue::from_static("claude-cli/2.1.280"));
        headers.insert("x-app", HeaderValue::from_static("cli"));
    }
    if request.model.reasoning
        && request.thinking != "off"
        && !request.model.compat_flag("forceAdaptiveThinking")
    {
        betas.push("interleaved-thinking-2025-05-14");
    }
    if !betas.is_empty()
        && let Ok(value) = HeaderValue::from_str(&betas.join(","))
    {
        headers.insert("anthropic-beta", value);
    }
    let body = build_body(&request, oauth);
    let response = post_sse(&http, url, headers, body, &cancel, &observer).await?;
    let mut payloads = response_events(response, cancel.clone());
    let mut blocks: Vec<Option<AssistantBlock>> = Vec::new();
    let mut args: Vec<String> = Vec::new();
    while let Some(payload) = payloads.next().await {
        let payload = payload?;
        let mut events = Vec::new();
        if payload == "[DONE]" {
            continue;
        }
        let item = decode_json(&payload)?;
        if let Some(index) = item.get("index").and_then(Value::as_u64)
            && index >= 1024
        {
            Err(crate::error::LlmError::Decode("provider block index is out of range".into()))?;
        }
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "error" => Err(crate::error::LlmError::Decode(item.to_string()))?,
            "message_start" => {
                if let Some(usage) = item.pointer("/message/usage") {
                    push_usage(usage, &mut events);
                }
            }
            "content_block_start" => {
                let index = event_index(&item)?;
                let block = item.get("content_block").cloned().unwrap_or(Value::Null);
                while blocks.len() <= index {
                    blocks.push(None);
                    args.push(String::new());
                }
                match block.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => {
                        events.push(StreamEvent::BlockStart {
                            index,
                            kind: BlockKind::Text,
                        });
                        blocks[index] = Some(AssistantBlock::Text {
                            text: block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                        });
                    }
                    "thinking" => {
                        events.push(StreamEvent::BlockStart {
                            index,
                            kind: BlockKind::Thinking,
                        });
                        let provider_data = block
                            .get("signature")
                            .cloned()
                            .map(|signature| json!({"signature":signature}));
                        blocks[index] = Some(AssistantBlock::Thinking {
                            text: block
                                .get("thinking")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            provider_data,
                        });
                    }
                    "redacted_thinking" => {
                        events.push(StreamEvent::BlockStart {
                            index,
                            kind: BlockKind::Thinking,
                        });
                        blocks[index] = Some(AssistantBlock::Thinking {
                            text: "[Reasoning redacted]".into(),
                            provider_data: Some(
                                json!({"redacted_thinking":block.get("data").cloned().unwrap_or(Value::Null)}),
                            ),
                        });
                    }
                    "tool_use" => {
                        let call_id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        events.push(StreamEvent::ToolCallStart {
                            index,
                            call_id: call_id.clone(),
                            name: name.clone(),
                        });
                        let initial = block
                            .get("input")
                            .filter(|v| !v.is_null())
                            .cloned()
                            .unwrap_or_else(|| json!({}));
                        args[index] = if initial == json!({}) {
                            String::new()
                        } else {
                            initial.to_string()
                        };
                        blocks[index] = Some(AssistantBlock::ToolCall {
                            call_id,
                            name,
                            args_json: args[index].clone(),
                        });
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let index = event_index(&item)?;
                let delta = item.get("delta").cloned().unwrap_or(Value::Null);
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        let text = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        if let Some(Some(AssistantBlock::Text { text: full })) =
                            blocks.get_mut(index)
                        {
                            full.push_str(&text);
                        }
                        events.push(StreamEvent::TextDelta { index, text });
                    }
                    "thinking_delta" => {
                        let text = delta
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        if let Some(Some(AssistantBlock::Thinking { text: full, .. })) =
                            blocks.get_mut(index)
                        {
                            full.push_str(&text);
                        }
                        events.push(StreamEvent::ThinkingDelta { index, text });
                    }
                    "signature_delta" => {
                        if let Some(Some(AssistantBlock::Thinking { provider_data, .. })) =
                            blocks.get_mut(index)
                        {
                            let data = provider_data.get_or_insert_with(|| json!({}));
                            let signature = data
                                .get("signature")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned();
                            *data = json!({"signature":format!("{signature}{}", delta.get("signature").and_then(Value::as_str).unwrap_or(""))});
                        }
                    }
                    "input_json_delta" => {
                        let addition = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if let Some(arguments) = args.get_mut(index) {
                            arguments.push_str(addition);
                        } else {
                            Err(crate::error::LlmError::Decode("tool arguments arrived before block start".into()))?;
                        }
                        if let Some(Some(AssistantBlock::ToolCall { args_json, .. })) =
                            blocks.get_mut(index)
                        {
                            args_json.push_str(addition);
                        }
                        events.push(StreamEvent::ToolArgsDelta {
                            index,
                            json: addition.to_owned(),
                        });
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = event_index(&item)?;
                if let Some(Some(block)) = blocks.get(index) {
                    events.push(StreamEvent::BlockEnd {
                        index,
                        block: block.clone(),
                    });
                }
            }
            "message_delta" => {
                if let Some(usage) = item.get("usage") {
                    push_usage(usage, &mut events);
                }
                let stop = match item
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .unwrap_or("end_turn")
                {
                    "tool_use" => StopReason::ToolUse,
                    "max_tokens" => StopReason::MaxTokens,
                    "refusal" => StopReason::Error("Provider refused the request".into()),
                    _ => StopReason::EndTurn,
                };
                events.push(StreamEvent::Stop(stop));
            }
            _ => {}
        }

        for event in events.drain(..) {
            yield event;
        }
    }
    })
}
fn event_index(item: &Value) -> Result<usize, crate::error::LlmError> {
    let index = item.get("index").and_then(Value::as_u64).unwrap_or(0);
    usize::try_from(index)
        .ok()
        .filter(|index| *index < 1024)
        .ok_or_else(|| {
            crate::error::LlmError::Decode("provider block index is out of range".into())
        })
}

fn build_body(request: &ChatRequest, oauth: bool) -> Value {
    let mut system = Vec::new();
    if oauth {
        system.push(json!({"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude."}));
    }
    for text in &request.system {
        system.push(json!({"type":"text","text":text}));
    }
    if let Some(last_system) = system.last_mut() {
        last_system["cache_control"] = json!({"type":"ephemeral"});
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
                if user_idx + 2 >= user_total && let Some(last) = blocks.last_mut() {
                    last["cache_control"] = json!({"type":"ephemeral"});
                }
                user_idx += 1;
                messages.push(json!({"role":"user","content":blocks}));
            }
            Message::Assistant { blocks } => messages.push(json!({"role":"assistant","content":blocks.iter().map(anthropic_block).collect::<Vec<_>>()})),
            Message::ToolResult { call_id, text, is_error, .. } => {
                user_idx += 1;
                let mut result = json!({"type":"tool_result","tool_use_id":call_id,"content":text,"is_error":is_error});
                if user_idx + 1 >= user_total { result["cache_control"] = json!({"type":"ephemeral"}); }
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
        if index + 1 == request.tools.len() { value["cache_control"] = json!({"type":"ephemeral"}); }
        value
    }).collect();
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    body
}

fn anthropic_block(block: &AssistantBlock) -> Value {
    match block {
        AssistantBlock::Text { text } => json!({"type":"text","text":text}),
        AssistantBlock::Thinking {
            text,
            provider_data,
        } => match provider_data {
            Some(data) if data.get("redacted_thinking").is_some() => {
                json!({"type":"redacted_thinking","data":data["redacted_thinking"]})
            }
            Some(data) if data.get("signature").is_some() => {
                json!({"type":"thinking","thinking":text,"signature":data["signature"]})
            }
            _ => json!({"type":"text","text":text}),
        },
        AssistantBlock::ToolCall {
            call_id,
            name,
            args_json,
        } => {
            json!({"type":"tool_use","id":call_id,"name":name,"input":serde_json::from_str::<Value>(args_json).unwrap_or_else(|_| json!({}))})
        }
    }
}

fn push_usage(value: &Value, output: &mut Vec<StreamEvent>) {
    output.push(StreamEvent::Usage(Usage {
        input: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read: value
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write: value
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }));
}
