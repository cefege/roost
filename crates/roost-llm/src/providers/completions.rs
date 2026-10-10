//! Ported from pi-ai 1.1.0 dist/api/openai-completions.js (MIT).
//! Handles OpenAI-compatible chat completions, including OpenRouter reasoning
//! effort, streamed tool argument assembly and provider reasoning replay data.

use futures::StreamExt;
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
        let url = format!("{}/v1/chat/completions", base.trim_end_matches('/'));
        let headers = auth_headers(&auth, "authorization", true)?;
        let body = build_body(&request);
        let response = post_sse(&http, url, headers, body, &cancel, &observer).await?;
        let mut payloads = response_events(response, cancel.clone());
        let mut blocks: Vec<Option<AssistantBlock>> = Vec::new();
        let mut tool_indices: Vec<Option<usize>> = Vec::new();
        while let Some(payload) = payloads.next().await {
            let payload = payload?;
            let mut events = Vec::new();
            if payload == "[DONE]" {
                continue;
            }
            let item = decode_json(&payload)?;
        let choice = item.pointer("/choices/0").cloned().unwrap_or(Value::Null);
        if let Some(usage) = item.get("usage") {
            push_usage(usage, &mut events);
        }
        if choice.is_null() {
            for event in events.drain(..) {
                yield event;
            }
            continue;
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            events.push(StreamEvent::Stop(match reason {
                "tool_calls" | "function_call" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                "content_filter" => {
                    StopReason::Error("Provider content filter stopped the response".into())
                }
                _ => StopReason::EndTurn,
            }));
        }
        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
        if let Some(text) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            let index = find_or_start(&mut blocks, &mut events, BlockKind::Text);
            if let Some(Some(AssistantBlock::Text { text: full })) = blocks.get_mut(index) {
                full.push_str(text);
            }
            events.push(StreamEvent::TextDelta {
                index,
                text: text.to_owned(),
            });
        }
        let reasoning = ["reasoning_content", "reasoning", "reasoning_text"]
            .iter()
            .find_map(|field| {
                delta
                    .get(*field)
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(|text| (*field, text))
            });
        if let Some((field, text)) = reasoning {
            let index = find_or_start(&mut blocks, &mut events, BlockKind::Thinking);
            if let Some(Some(AssistantBlock::Thinking {
                text: full,
                provider_data,
            })) = blocks.get_mut(index)
            {
                full.push_str(text);
                *provider_data = Some(json!({"field":field}));
            }
            events.push(StreamEvent::ThinkingDelta {
                index,
                text: text.to_owned(),
            });
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let stream_index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if stream_index >= 1024 {
                    Err(crate::error::LlmError::Decode("provider tool index is out of range".into()))?;
                }
                while tool_indices.len() <= stream_index {
                    tool_indices.push(None);
                }
                let block_index = if let Some(index) = tool_indices[stream_index] {
                    index
                } else {
                    let index = blocks.len();
                    let id = call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let name = call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    events.push(StreamEvent::ToolCallStart {
                        index,
                        call_id: id.clone(),
                        name: name.clone(),
                    });
                    blocks.push(Some(AssistantBlock::ToolCall {
                        call_id: id,
                        name,
                        args_json: String::new(),
                    }));
                    tool_indices[stream_index] = Some(index);
                    index
                };
                if let Some(Some(AssistantBlock::ToolCall {
                    call_id,
                    name,
                    args_json,
                })) = blocks.get_mut(block_index)
                {
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        call_id.clone_from(&id.to_owned());
                    }
                    if let Some(next_name) = call.pointer("/function/name").and_then(Value::as_str)
                    {
                        name.clone_from(&next_name.to_owned());
                    }
                    if let Some(next_args) =
                        call.pointer("/function/arguments").and_then(Value::as_str)
                    {
                        args_json.push_str(next_args);
                        events.push(StreamEvent::ToolArgsDelta {
                            index: block_index,
                            json: next_args.to_owned(),
                        });
                    }
                }
            }
        }
        if let Some(details) = delta.get("reasoning_details").and_then(Value::as_array)
            && !details.is_empty()
        {
            let index = find_or_start(&mut blocks, &mut events, BlockKind::Thinking);
            if let Some(Some(AssistantBlock::Thinking { provider_data, .. })) =
                blocks.get_mut(index)
            {
                *provider_data = Some(json!({"reasoning_details":details}));
            }
        }
        for event in events.drain(..) {
            yield event;
        }
    }
    for (index, block) in blocks.iter().enumerate() {
        if let Some(block) = block {
            yield StreamEvent::BlockEnd {
                index,
                block: block.clone(),
            };
        }
    }
    })
}

fn build_body(request: &ChatRequest) -> Value {
    let mut messages = Vec::new();
    for message in &request.messages {
        match message {
            Message::User { content } => {
                let content: Vec<Value> = content.iter().map(|part| match part {
                    crate::message::UserContent::Text { text } => json!({"type":"text","text":text}),
                    crate::message::UserContent::Image { mime, base64 } => json!({"type":"image_url","image_url":{"url":format!("data:{mime};base64,{base64}")}}),
                }).collect();
                messages.push(json!({"role":"user","content":content}));
            }
            Message::Assistant { blocks } => {
                let mut message = json!({"role":"assistant"});
                let text = blocks
                    .iter()
                    .filter_map(|block| match block {
                        AssistantBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !text.is_empty() {
                    message["content"] = json!(text);
                }
                let calls: Vec<Value> = blocks.iter().filter_map(|block| match block {
                    AssistantBlock::ToolCall { call_id, name, args_json } => Some(json!({"id":call_id,"type":"function","function":{"name":name,"arguments":args_json}})),
                    _ => None,
                }).collect();
                if !calls.is_empty() {
                    message["tool_calls"] = json!(calls);
                }
                if let Some(reasoning) = blocks.iter().find_map(|block| match block {
                    AssistantBlock::Thinking {
                        provider_data: Some(data),
                        ..
                    } => Some(data),
                    _ => None,
                }) {
                    if let Some(details) = reasoning.get("reasoning_details") {
                        message["reasoning_details"] = details.clone();
                    }
                    if let Some(field) = reasoning.get("field").and_then(Value::as_str) {
                        message[field] = blocks
                            .iter()
                            .find_map(|block| match block {
                                AssistantBlock::Thinking { text, .. } => Some(text.as_str()),
                                _ => None,
                            })
                            .map_or(Value::Null, |text| json!(text));
                    }
                }
                messages.push(message);
            }
            Message::ToolResult { call_id, text, .. } => {
                messages.push(json!({"role":"tool","tool_call_id":call_id,"content":text}))
            }
        }
    }
    let mut all_messages = Vec::new();
    for text in &request.system {
        all_messages.push(json!({"role":"system","content":text}));
    }
    all_messages.extend(messages);
    let tools: Vec<Value> = request.tools.iter().map(|tool| json!({"type":"function","function":{"name":tool.name,"description":tool.description,"parameters":tool.parameters}})).collect();
    let mut body = json!({"model":request.model.id,"messages":all_messages,"stream":true,"stream_options":{"include_usage":true},"max_tokens":request.max_tokens.unwrap_or(request.model.max_tokens)});
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        body["tool_choice"] = json!("auto");
    }
    if request.model.reasoning
        && request.thinking != "off"
        && let Some(effort) = request.model.provider_thinking_value(&request.thinking)
    {
        if request.model.provider == "openrouter" {
            body["reasoning"] = json!({"effort":effort});
        } else {
            body["reasoning_effort"] = json!(effort);
        }
    }
    body
}

fn find_or_start(
    blocks: &mut Vec<Option<AssistantBlock>>,
    events: &mut Vec<StreamEvent>,
    kind: BlockKind,
) -> usize {
    if let Some(index) = blocks.iter().position(|block| {
        matches!(
            (kind, block),
            (BlockKind::Text, Some(AssistantBlock::Text { .. }))
                | (BlockKind::Thinking, Some(AssistantBlock::Thinking { .. }))
        )
    }) {
        return index;
    }
    let index = blocks.len();
    events.push(StreamEvent::BlockStart { index, kind });
    blocks.push(Some(match kind {
        BlockKind::Text => AssistantBlock::Text {
            text: String::new(),
        },
        BlockKind::Thinking => AssistantBlock::Thinking {
            text: String::new(),
            provider_data: None,
        },
        BlockKind::ToolCall => AssistantBlock::ToolCall {
            call_id: String::new(),
            name: String::new(),
            args_json: String::new(),
        },
    }));
    index
}

fn push_usage(value: &Value, output: &mut Vec<StreamEvent>) {
    output.push(StreamEvent::Usage(Usage {
        input: value
            .get("prompt_tokens")
            .or_else(|| value.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: value
            .get("completion_tokens")
            .or_else(|| value.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read: value
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write: 0,
    }));
}
