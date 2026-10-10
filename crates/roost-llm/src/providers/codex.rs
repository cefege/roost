//! Ported from pi-ai 1.1.0 dist/api/openai-codex-responses.js (MIT).
//! Sends the Codex Responses API request with account affinity and instructions,
//! and preserves encrypted reasoning content in assistant thinking metadata.

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
    let url = format!("{}/codex/responses", base.trim_end_matches('/'));
    let mut headers = auth_headers(&auth, "authorization", true)?;
    if let ResolvedAuth::OAuth { account_id: Some(account_id), .. } = &auth {
        headers.insert("chatgpt-account-id", HeaderValue::from_str(account_id).map_err(|error| crate::error::LlmError::Auth(error.to_string()))?);
    }
    headers.insert("openai-beta", HeaderValue::from_static("responses=experimental"));
    let body = build_body(&request);
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
        if let Some(index) = item.get("output_index").and_then(Value::as_u64)
            && index >= 1024
        {
            Err(crate::error::LlmError::Decode("provider block index is out of range".into()))?;
        }
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.created" => {}
            "response.output_item.added" => {
                let index = item
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let output = item.get("item").cloned().unwrap_or(Value::Null);
                while blocks.len() <= index {
                    blocks.push(None);
                    args.push(String::new());
                }
                match output.get("type").and_then(Value::as_str).unwrap_or("") {
                    "message" => {
                        events.push(StreamEvent::BlockStart {
                            index,
                            kind: BlockKind::Text,
                        });
                        blocks[index] = Some(AssistantBlock::Text {
                            text: String::new(),
                        });
                    }
                    "reasoning" => {
                        events.push(StreamEvent::BlockStart {
                            index,
                            kind: BlockKind::Thinking,
                        });
                        blocks[index] = Some(AssistantBlock::Thinking {
                            text: String::new(),
                            provider_data: None,
                        });
                    }
                    "function_call" => {
                        let call_id = output
                            .get("call_id")
                            .or_else(|| output.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        let name = output
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        events.push(StreamEvent::ToolCallStart {
                            index,
                            call_id: call_id.clone(),
                            name: name.clone(),
                        });
                        let initial = output
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        args[index] = initial.to_owned();
                        blocks[index] = Some(AssistantBlock::ToolCall {
                            call_id,
                            name,
                            args_json: initial.to_owned(),
                        });
                    }
                    _ => {}
                }
            }
            "response.output_text.delta" => {
                let index = item
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let text = item
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if let Some(Some(AssistantBlock::Text { text: full })) = blocks.get_mut(index) {
                    full.push_str(&text);
                }
                events.push(StreamEvent::TextDelta { index, text });
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let index = item
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let text = item
                    .get("delta")
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
            "response.reasoning_summary_text.done" | "response.reasoning_text.done" => {
                let index = item
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                if let Some(Some(AssistantBlock::Thinking { text, .. })) = blocks.get_mut(index)
                    && text.is_empty()
                {
                    *text = item
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                }
            }
            "response.reasoning_summary_part.added" => {}
            "response.function_call_arguments.delta" => {
                let index = item
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let delta = item.get("delta").and_then(Value::as_str).unwrap_or("");
                if let Some(args) = args.get_mut(index) {
                    args.push_str(delta);
                }
                if let Some(Some(AssistantBlock::ToolCall { args_json, .. })) =
                    blocks.get_mut(index)
                {
                    args_json.push_str(delta);
                }
                events.push(StreamEvent::ToolArgsDelta {
                    index,
                    json: delta.to_owned(),
                });
            }
            "response.function_call_arguments.done" => {
                let index = item
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                if let Some(Some(AssistantBlock::ToolCall { args_json, .. })) =
                    blocks.get_mut(index)
                    && args_json.is_empty()
                {
                    *args_json = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .to_owned();
                }
            }
            "response.output_item.done" => {
                let index = item
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let output = item.get("item").cloned().unwrap_or(Value::Null);
                if let Some(Some(AssistantBlock::Thinking { provider_data, .. })) =
                    blocks.get_mut(index)
                {
                    let encrypted = output
                        .get("encrypted_content")
                        .or_else(|| output.get("summary"))
                        .cloned();
                    if let Some(encrypted) = encrypted {
                        *provider_data = Some(json!({"encrypted_content":encrypted}));
                    }
                }
                if let Some(Some(block)) = blocks.get(index) {
                    events.push(StreamEvent::BlockEnd {
                        index,
                        block: block.clone(),
                    });
                }
            }
            "response.completed" | "response.done" => {
                let response = item.get("response").cloned().unwrap_or(Value::Null);
                let usage = response.get("usage").cloned().unwrap_or(Value::Null);
                events.push(StreamEvent::Usage(Usage {
                    input: usage
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    output: usage
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    cache_read: usage
                        .pointer("/input_tokens_details/cached_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    cache_write: 0,
                }));
                let reason = match response
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed")
                {
                    "incomplete" => StopReason::MaxTokens,
                    "failed" => StopReason::Error(
                        response
                            .pointer("/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("Codex response failed")
                            .to_owned(),
                    ),
                    _ if response
                        .get("output")
                        .and_then(Value::as_array)
                        .is_some_and(|items| {
                            items.iter().any(|entry| {
                                entry.get("type").and_then(Value::as_str) == Some("function_call")
                            })
                        }) =>
                    {
                        StopReason::ToolUse
                    }
                    _ => StopReason::EndTurn,
                };
                events.push(StreamEvent::Stop(reason));
            }
            "response.failed" | "error" => {
                Err(crate::error::LlmError::Decode(item.to_string()))?;
            }
            _ => {}
        }
        for event in events.drain(..) {
            yield event;
        }
    }
    })
}

fn build_body(request: &ChatRequest) -> Value {
    let mut input = Vec::new();
    for message in &request.messages {
        match message {
            Message::User { content } => {
                for part in content {
                    match part {
                    crate::message::UserContent::Text { text } => input.push(json!({"role":"user","content":[{"type":"input_text","text":text}]})),
                    crate::message::UserContent::Image { mime, base64 } => input.push(json!({"role":"user","content":[{"type":"input_image","image_url":format!("data:{mime};base64,{base64}")}]})),
                }
                }
            }
            Message::Assistant { blocks } => {
                for block in blocks {
                    match block {
                    AssistantBlock::Text { text } => input.push(json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})),
                    AssistantBlock::Thinking { provider_data, .. } => if let Some(encrypted) = provider_data.as_ref().and_then(|data| data.get("encrypted_content")) { input.push(json!({"type":"reasoning","encrypted_content":encrypted})); },
                    AssistantBlock::ToolCall { call_id, name, args_json } => input.push(json!({"type":"function_call","call_id":call_id,"name":name,"arguments":args_json})),
                }
                }
            }
            Message::ToolResult { call_id, text, .. } => {
                input.push(json!({"type":"function_call_output","call_id":call_id,"output":text}))
            }
        }
    }
    let tools: Vec<Value> = request.tools.iter().map(|tool| json!({"type":"function","name":tool.name,"description":tool.description,"parameters":tool.parameters,"strict":false})).collect();
    let instructions = request.system.join("\n\n");
    let instructions = if instructions.is_empty() {
        "You are a helpful assistant.".to_owned()
    } else {
        instructions
    };
    let mut body = json!({"model":request.model.id,"store":false,"stream":true,"instructions":instructions,"input":input,"include":["reasoning.encrypted_content"],"prompt_cache_key":request.session_id,"parallel_tool_calls":true,"tool_choice":"auto","max_output_tokens":request.max_tokens.unwrap_or(request.model.max_tokens)});
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if request.model.reasoning {
        let effort = request
            .model
            .provider_thinking_value(&request.thinking)
            .unwrap_or_else(|| "none".into());
        body["reasoning"] = json!({"effort":effort,"summary":"auto"});
    }
    body
}
