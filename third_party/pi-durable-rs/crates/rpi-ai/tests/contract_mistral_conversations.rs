//! Contract tests for the mistral-conversations adapter: drive `stream()` /
//! `stream_simple` over a scripted local HTTP server with recorded SSE
//! streams, and assert both sides of the contract — the request shape
//! (method / path / key headers / body JSON) and the emitted `StreamEvent`
//! sequence. Mirrors `contract_adapters.rs`; the SSE payloads are recorded in
//! the upstream Mistral wire format (native transport since 9dd90a497).
//!
//! The `mistral_http_transport_*` tests port
//! `packages/ai/test/mistral-http-transport.test.ts` @ 4181f66 (added by
//! 9dd90a497), one Rust test per upstream `it(...)`, with the injected-fetch
//! mocks replaced by the scripted server (coding standards §12.4).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use rpi_ai::api::mistral_conversations::{
    MistralConversations, MistralOptions, MistralPromptMode, MistralToolChoice,
    stream as stream_mistral,
};
use rpi_ai::models::ProviderStreams;
use rpi_ai::types::{
    ApiKind, CacheRetention, Context, Message, Model, ProviderHeaders, SimpleStreamOptions,
    StopReason, StreamEvent, StreamOptions, ThinkingLevel,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Scripted HTTP server
// ---------------------------------------------------------------------------

/// One captured request: request line, headers (lowercased names) and body.
#[derive(Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl CapturedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn body_json(&self) -> Value {
        serde_json::from_str(&self.body).expect("request body is JSON")
    }
}

/// A scripted response for [`serve_responses`].
enum ScriptedResponse {
    /// Complete response written in one go, with extra headers allowed.
    Full {
        status: u16,
        headers: Vec<(&'static str, &'static str)>,
        body: &'static str,
    },
    /// 200 SSE response dribbled one byte per write, so transport chunks
    /// split SSE frames and UTF-8 sequences arbitrarily
    /// (`createBytewiseSseResponse` upstream).
    Dribble(&'static str),
    /// 200 response headers, then nothing: the body never arrives and the
    /// socket is held until the client disconnects (abort/timeout tests).
    Hang,
}

/// Serves the scripted responses, one per connection, on a loopback port.
/// Returns the base URL and a channel of captured requests.
async fn serve_responses(
    script: Vec<ScriptedResponse>,
) -> (String, mpsc::Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel(script.len().max(1));
    tokio::spawn(async move {
        for response in script {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let request = read_request(&mut socket).await;
            tx.send(request).await.expect("send captured request");
            match response {
                ScriptedResponse::Full {
                    status,
                    headers,
                    body,
                } => {
                    let reason = match status {
                        200 => "OK",
                        400 => "Bad Request",
                        403 => "Forbidden",
                        429 => "Too Many Requests",
                        _ => "Status",
                    };
                    let content_type = if status == 200 {
                        "text/event-stream"
                    } else {
                        "application/json"
                    };
                    let mut head = format!(
                        "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n",
                        body.len()
                    );
                    for (name, value) in headers {
                        head.push_str(&format!("{name}: {value}\r\n"));
                    }
                    socket
                        .write_all(format!("{head}\r\n{body}").as_bytes())
                        .await
                        .expect("write response");
                }
                ScriptedResponse::Dribble(body) => {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    socket.write_all(head.as_bytes()).await.expect("write head");
                    for byte in body.as_bytes() {
                        if socket.write_all(&[*byte]).await.is_err() {
                            break;
                        }
                    }
                }
                ScriptedResponse::Hang => {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: keep-alive\r\n\r\n",
                        )
                        .await
                        .expect("write head");
                    // Hold the socket open until the client goes away.
                    let mut buf = [0u8; 1024];
                    loop {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                }
            }
        }
    });
    (format!("http://{addr}"), rx)
}

/// Serves the scripted `(status, body)` responses, one per connection, on a
/// loopback port. Returns the base URL and a channel of captured requests.
async fn serve(script: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<CapturedRequest>) {
    serve_responses(
        script
            .into_iter()
            .map(|(status, body)| ScriptedResponse::Full {
                status,
                headers: Vec::new(),
                body,
            })
            .collect(),
    )
    .await
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> CapturedRequest {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = socket.read(&mut chunk).await.expect("read");
        if n == 0 {
            panic!("connection closed before headers complete");
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subslice(&buffer, b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().expect("request line");
    let mut parts = request_line.split(' ');
    let method = parts.next().expect("method").to_owned();
    let path = parts.next().expect("path").to_owned();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_lowercase();
        let value = value.trim().to_owned();
        if name == "content-length" {
            content_length = value.parse().expect("content-length");
        }
        headers.push((name, value));
    }
    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        let n = socket.read(&mut chunk).await.expect("read body");
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    CapturedRequest {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body[..content_length]).to_string(),
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn model(id: &str, base_url: &str, extra: Value) -> Model {
    let mut value = json!({
        "id": id, "name": id, "api": ApiKind::MISTRAL_CONVERSATIONS, "provider": "mistral",
        "baseUrl": base_url, "reasoning": false, "input": ["text"],
        "cost": {"input": 2.0, "output": 6.0, "cacheRead": 0.2, "cacheWrite": 2.0},
        "contextWindow": 128000, "maxTokens": 8192
    });
    value
        .as_object_mut()
        .expect("object")
        .extend(extra.as_object().cloned().unwrap_or_default());
    serde_json::from_value(value).expect("model")
}

fn context(messages: Vec<Message>) -> rpi_ai::types::TranscriptContext {
    rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: None,
        messages,
        tools: None,
    })
}

fn user_text(text: &str) -> Message {
    serde_json::from_value(json!({"role": "user", "content": text, "timestamp": 0})).expect("user")
}

fn options() -> StreamOptions {
    StreamOptions {
        request: rpi_ai::ProviderRequestOptions {
            api_key: Some("test-key".to_owned()),
            ..Default::default()
        },
        ..StreamOptions::default()
    }
}

fn simple_options(reasoning: Option<ThinkingLevel>, stream: StreamOptions) -> SimpleStreamOptions {
    SimpleStreamOptions {
        stream,
        reasoning,
        thinking_budgets: None,
        tool_choice: None,
    }
}

fn event_kinds(events: &[StreamEvent]) -> Vec<&str> {
    events
        .iter()
        .map(|event| match event {
            StreamEvent::Start { .. } => "start",
            StreamEvent::TextStart { .. } => "text_start",
            StreamEvent::TextDelta { .. } => "text_delta",
            StreamEvent::TextEnd { .. } => "text_end",
            StreamEvent::ThinkingStart { .. } => "thinking_start",
            StreamEvent::ThinkingDelta { .. } => "thinking_delta",
            StreamEvent::ThinkingEnd { .. } => "thinking_end",
            StreamEvent::ToolCallStart { .. } => "toolcall_start",
            StreamEvent::ToolCallDelta { .. } => "toolcall_delta",
            StreamEvent::ToolCallEnd { .. } => "toolcall_end",
            StreamEvent::Done { .. } => "done",
            StreamEvent::Error { .. } => "error",
        })
        .collect()
}

async fn collect(
    stream: rpi_ai::utils::event_stream::AssistantMessageEventStream,
) -> Vec<StreamEvent> {
    tokio::time::timeout(Duration::from_secs(10), stream.collect())
        .await
        .expect("stream completes within 10s")
}

// ---------------------------------------------------------------------------
// Recorded SSE streams
// ---------------------------------------------------------------------------

const TEXT_SSE: &str = concat!(
    "data: {\"id\":\"cmpl-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"cmpl-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"cmpl-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15,\"prompt_tokens_details\":{\"cached_tokens\":4}}}\n",
    "\n",
    "data: [DONE]\n",
    "\n",
);

const THINKING_SSE: &str = concat!(
    "data: {\"id\":\"cmpl-2\",\"model\":\"magistral-medium-latest\",\"choices\":[{\"index\":0,\"delta\":{\"content\":[{\"type\":\"thinking\",\"thinking\":[{\"type\":\"text\",\"text\":\"hmm\"}]}]},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"cmpl-2\",\"model\":\"magistral-medium-latest\",\"choices\":[{\"index\":0,\"delta\":{\"content\":[{\"type\":\"text\",\"text\":\"answer\"}]},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":3,\"total_tokens\":11}}\n",
    "\n",
    "data: [DONE]\n",
    "\n",
);

const TOOL_CALL_SSE: &str = concat!(
    "data: {\"id\":\"cmpl-3\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"abc123XYZ\",\"type\":\"function\",\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"cmpl-3\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"abc123XYZ\",\"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"cmd\\\":\"}}]},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"cmpl-3\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"abc123XYZ\",\"function\":{\"name\":\"bash\",\"arguments\":\"\\\"ls\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n",
    "\n",
    "data: [DONE]\n",
    "\n",
);

// ---------------------------------------------------------------------------
// Contract tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_mistral_conversations_contract() {
    let (base_url, mut captured) = serve(vec![(200, TEXT_SSE)]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));
    let mut opts = options();
    opts.session_id = Some("session-123".to_owned());
    let events =
        collect(MistralConversations.stream(&m, &context(vec![user_text("hi")]), Some(opts))).await;

    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer test-key"));
    assert_eq!(request.header("accept"), Some("text/event-stream"));
    // Session-based prompt caching: header + body key (mistral-conversations.ts:231-235,262).
    assert_eq!(request.header("x-affinity"), Some("session-123"));
    let body = request.body_json();
    assert_eq!(body["model"], json!("mistral-large-latest"));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["prompt_cache_key"], json!("session-123"));
    assert_eq!(body["messages"][0]["role"], json!("user"));

    assert_eq!(
        event_kinds(&events),
        vec![
            "start",
            "text_start",
            "text_delta",
            "text_delta",
            "text_end",
            "done"
        ]
    );
    let StreamEvent::Done { reason, message } = &events[5] else {
        panic!("expected done event");
    };
    assert_eq!(*reason, rpi_ai::types::DoneReason::Stop);
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.response_id.as_deref(), Some("cmpl-1"));
    // Cached tokens split out of input (mistral-conversations.ts:337-348).
    assert_eq!(message.usage.input, 6);
    assert_eq!(message.usage.cache_read, 4);
    assert_eq!(message.usage.cache_write, 0);
    assert_eq!(message.usage.output, 5);
    assert_eq!(message.usage.total_tokens, 15);
}

#[tokio::test]
async fn test_mistral_prompt_mode_reasoning_stream() {
    // mistral-reasoning-mode.test.ts intent: Magistral models stream with
    // `prompt_mode: "reasoning"` and produce thinking blocks.
    let (base_url, mut captured) = serve(vec![(200, THINKING_SSE)]).await;
    let m = model(
        "magistral-medium-latest",
        &base_url,
        json!({"reasoning": true}),
    );
    let events = collect(
        MistralConversations
            .stream_simple(
                &m,
                &context(vec![user_text("hi")]),
                Some(simple_options(Some(ThinkingLevel::Medium), options())),
            )
            .expect("stream_simple"),
    )
    .await;

    let request = captured.recv().await.expect("request captured");
    let body = request.body_json();
    assert_eq!(body["prompt_mode"], json!("reasoning"));
    assert!(body.get("reasoning_effort").is_none());
    // No session id: no prompt caching.
    assert!(body.get("prompt_cache_key").is_none());
    assert!(request.header("x-affinity").is_none());

    assert_eq!(
        event_kinds(&events),
        vec![
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "text_start",
            "text_delta",
            "text_end",
            "done"
        ]
    );
}

#[tokio::test]
async fn test_mistral_reasoning_effort_stream() {
    // mistral-reasoning-mode.test.ts intent: models with a thinking level
    // map use `reasoning_effort` instead of `prompt_mode` (#9678); the
    // vendored catalog supplies the maps for Small 4 / Medium 3.5.
    let (base_url, mut captured) = serve(vec![(200, TEXT_SSE)]).await;
    let m = model(
        "mistral-small-2603",
        &base_url,
        json!({
            "reasoning": true,
            "thinkingLevelMap": {
                "off": "none", "minimal": null, "low": null,
                "medium": null, "high": "high", "xhigh": null, "max": null
            }
        }),
    );
    let events = collect(
        MistralConversations
            .stream_simple(
                &m,
                &context(vec![user_text("hi")]),
                Some(simple_options(Some(ThinkingLevel::Medium), options())),
            )
            .expect("stream_simple"),
    )
    .await;

    let request = captured.recv().await.expect("request captured");
    let body = request.body_json();
    assert_eq!(body["reasoning_effort"], json!("high"));
    assert!(body.get("prompt_mode").is_none());
    assert_eq!(event_kinds(&events).last(), Some(&"done"));
}

#[tokio::test]
async fn test_mistral_tool_call_stream_and_id_normalization() {
    let (base_url, mut captured) = serve(vec![(200, TOOL_CALL_SSE)]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));

    // Cross-provider history: the OpenAI tool-call id is normalized to the
    // 9-char alphanumeric Mistral shape, and the tool result id back-filled
    // (mistral-conversations.ts:157-187 via transformMessages).
    let history: Vec<Message> = serde_json::from_value(json!([
        {
            "role": "assistant", "api": "openai-completions", "provider": "openai",
            "model": "gpt-4o", "timestamp": 0, "stopReason": "toolUse",
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                      "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "content": [{"type": "toolCall", "id": "call_long_openai_id_with_underscores", "name": "bash", "arguments": {"cmd": "ls"}}]
        },
        {
            "role": "toolResult", "toolCallId": "call_long_openai_id_with_underscores",
            "toolName": "bash", "content": [{"type": "text", "text": "ok"}],
            "isError": false, "timestamp": 0
        },
        {"role": "user", "content": "again", "timestamp": 0}
    ]))
    .expect("history");

    let events = collect(MistralConversations.stream(&m, &context(history), Some(options()))).await;

    let request = captured.recv().await.expect("request captured");
    let body = request.body_json();
    let normalized_id = body["messages"][0]["tool_calls"][0]["id"]
        .as_str()
        .expect("tool call id");
    assert_eq!(normalized_id.len(), 9);
    assert!(normalized_id.chars().all(|c| c.is_ascii_alphanumeric()));
    // The tool result carries the same normalized id.
    assert_eq!(body["messages"][1]["tool_call_id"], json!(normalized_id));
    assert_eq!(body["messages"][1]["role"], json!("tool"));

    assert_eq!(
        event_kinds(&events),
        vec![
            "start",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_end",
            "done"
        ]
    );
    let StreamEvent::Done { reason, message } = events.last().expect("done") else {
        panic!("expected done event");
    };
    assert_eq!(*reason, rpi_ai::types::DoneReason::ToolUse);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    let rpi_ai::types::AssistantContent::ToolCall(call) = &message.content[0] else {
        panic!("expected tool call block");
    };
    assert_eq!(call.id, "abc123XYZ");
    assert_eq!(call.name, "bash");
    assert_eq!(
        serde_json::Value::Object(call.arguments.clone()),
        json!({"cmd": "ls"})
    );
}

#[tokio::test]
async fn test_mistral_error_stream() {
    let error_body = "{\"message\":\"Invalid model\",\"type\":\"invalid_request_error\"}";
    let (base_url, mut captured) = serve(vec![(400, error_body)]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));
    let events =
        collect(MistralConversations.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;

    captured.recv().await.expect("request captured");
    assert_eq!(event_kinds(&events), vec!["error"]);
    let StreamEvent::Error { reason, error } = &events[0] else {
        panic!("expected error event");
    };
    assert_eq!(*reason, rpi_ai::types::ErrorReason::Error);
    assert_eq!(error.stop_reason, StopReason::Error);
    // formatMistralError: status + trimmed body (mistral-conversations.ts:189-201).
    assert_eq!(
        error.error_message,
        Some(format!("Mistral API error (400): {error_body}"))
    );
}

// ---------------------------------------------------------------------------
// Raw stop reasons (mistral-raw-stop-reason.test.ts: 5a53f086e, text unified
// by 5a2539a7b @ 4181f66)
// ---------------------------------------------------------------------------

/// mistral-raw-stop-reason.test.ts: "preserves raw Mistral finish reasons for
/// successful stops".
#[tokio::test]
async fn preserves_raw_mistral_finish_reasons_for_successful_stops() {
    let (base_url, mut captured) = serve(vec![(200, TEXT_SSE)]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));
    let events =
        collect(MistralConversations.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;

    captured.recv().await.expect("request captured");
    let Some(StreamEvent::Done { message, .. }) = events.last() else {
        panic!("expected done event, got {events:?}");
    };
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("stop"));
    assert_eq!(message.error_message, None);
}

/// mistral-raw-stop-reason.test.ts: "preserves raw Mistral finish reasons for
/// provider error stops".
#[tokio::test]
async fn preserves_raw_mistral_finish_reasons_for_provider_error_stops() {
    const ERROR_SSE: &str = concat!(
        "data: {\"id\":\"mistral-response-id\",\"choices\":[{\"finish_reason\":\"error\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":0,\"total_tokens\":1}}\n",
        "\n",
        "data: [DONE]\n",
        "\n",
    );
    let (base_url, mut captured) = serve(vec![(200, ERROR_SSE)]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));
    let events =
        collect(MistralConversations.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;

    captured.recv().await.expect("request captured");
    let Some(StreamEvent::Error { error, .. }) = events.last() else {
        panic!("expected error event, got {events:?}");
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.raw_stop_reason.as_deref(), Some("error"));
    assert_eq!(
        error.error_message.as_deref(),
        // #10487 (7fb59f995): "server error" makes the message retryable.
        Some("Provider stopped with: error (server error)")
    );
}

/// mistral-raw-stop-reason.test.ts: "treats unknown Mistral finish reasons as
/// provider error stops".
#[tokio::test]
async fn treats_unknown_mistral_finish_reasons_as_provider_error_stops() {
    const UNMAPPED_SSE: &str = concat!(
        "data: {\"id\":\"mistral-response-id\",\"choices\":[{\"finish_reason\":\"unmapped_error\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":0,\"total_tokens\":1}}\n",
        "\n",
        "data: [DONE]\n",
        "\n",
    );
    let (base_url, mut captured) = serve(vec![(200, UNMAPPED_SSE)]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));
    let events =
        collect(MistralConversations.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;

    captured.recv().await.expect("request captured");
    let Some(StreamEvent::Error { error, .. }) = events.last() else {
        panic!("expected error event, got {events:?}");
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.raw_stop_reason.as_deref(), Some("unmapped_error"));
    assert_eq!(
        error.error_message.as_deref(),
        Some("Provider stopped with: unmapped_error")
    );
}

#[tokio::test]
async fn test_mistral_cache_retention_none_omits_prompt_caching() {
    // mistral-reasoning-mode.test.ts intent: cacheRetention "none" omits both
    // the promptCacheKey body field and the x-affinity header.
    let (base_url, mut captured) = serve(vec![(200, TEXT_SSE)]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));
    let mut opts = options();
    opts.session_id = Some("session-123".to_owned());
    opts.cache_retention = Some(CacheRetention::None);
    let events =
        collect(MistralConversations.stream(&m, &context(vec![user_text("hi")]), Some(opts))).await;

    let request = captured.recv().await.expect("request captured");
    assert!(request.header("x-affinity").is_none());
    let body = request.body_json();
    assert!(body.get("prompt_cache_key").is_none());
    assert_eq!(event_kinds(&events).last(), Some(&"done"));
}

#[tokio::test]
async fn test_mistral_tool_schema_strict_serialization() {
    // mistral-tool-schema.test.ts intent: a `json_schema`/`require`
    // constrained-sampling tool serializes with `"strict": true` and plain
    // object parameters (no TypeBox symbol keys — impossible in Rust).
    let (base_url, mut captured) = serve(vec![(200, TEXT_SSE)]).await;
    let m = model("devstral-medium-latest", &base_url, json!({}));
    let tool: rpi_ai::types::Tool = serde_json::from_value(json!({
        "name": "inspect_schema",
        "description": "Inspect the schema",
        "parameters": {"type": "object", "properties": {"nested": {"type": "object", "properties": {"value": {"type": "string"}}}}},
        "constrainedSampling": {"type": "json_schema", "strict": "require"}
    }))
    .expect("tool");
    let ctx = rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: None,
        messages: vec![user_text("Hi")],
        tools: Some(vec![tool]),
    });
    let events = collect(MistralConversations.stream(&m, &ctx, Some(options()))).await;

    let request = captured.recv().await.expect("request captured");
    let body = request.body_json();
    let tools = body["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], json!("function"));
    assert_eq!(tools[0]["function"]["strict"], json!(true));
    assert_eq!(
        tools[0]["function"]["parameters"]["properties"]["nested"]["properties"]["value"]["type"],
        json!("string")
    );
    assert_eq!(event_kinds(&events).last(), Some(&"done"));
}

// ---------------------------------------------------------------------------
// mistral-http-transport.test.ts @ 4181f66 (added by 9dd90a497, "fix(ai):
// replace Mistral SDK with native transport"). One Rust test per upstream
// `it(...)`; the injected-fetch mocks become the scripted server.
// ---------------------------------------------------------------------------

/// Upstream `createTerminalEvent()` + `[DONE]`, framed with CRLF boundaries
/// like `createSseResponse`.
const TERMINAL_SSE: &str = concat!(
    "data: {\"id\":\"mistral-response-id\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\r\n",
    "\r\n",
    "data: [DONE]\r\n",
    "\r\n",
);

/// mistral-http-transport.test.ts: "serializes SDK-style payloads to the
/// Mistral wire format".
#[tokio::test]
async fn mistral_http_transport_serializes_sdk_style_payloads_to_the_mistral_wire_format() {
    let (base_url, mut captured) = serve_responses(vec![ScriptedResponse::Full {
        status: 200,
        headers: vec![("x-request-id", "request-1")],
        body: TERMINAL_SSE,
    }])
    .await;
    let m = model(
        "mistral-large-latest",
        &base_url,
        json!({"input": ["text", "image"]}),
    );
    let ctx = rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: Some("Be precise".to_owned()),
        messages: vec![
            serde_json::from_value(json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "describe"},
                    {"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"},
                ],
                "timestamp": 1
            }))
            .expect("user"),
        ],
        tools: Some(vec![
            serde_json::from_value(json!({
                "name": "lookup",
                "description": "Look something up",
                "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}
            }))
            .expect("tool"),
        ]),
    });

    let captured_payload = Arc::new(Mutex::new(None::<Value>));
    let captured_response = Arc::new(Mutex::new(None::<(u16, _)>));
    let mut opts = MistralOptions {
        stream: StreamOptions {
            request: rpi_ai::ProviderRequestOptions {
                api_key: Some("secret".to_owned()),
                ..Default::default()
            },
            ..StreamOptions::default()
        },
        tool_choice: Some(MistralToolChoice::Function {
            name: "lookup".to_owned(),
        }),
        prompt_mode: Some(MistralPromptMode::Reasoning),
        reasoning_effort: Some("high".to_owned()),
    };
    opts.stream.headers = Some(ProviderHeaders::from([(
        "x-custom".to_owned(),
        Some("value".to_owned()),
    )]));
    opts.stream.max_tokens = Some(123);
    opts.stream.session_id = Some("session-1".to_owned());
    let payload_slot = captured_payload.clone();
    opts.stream.on_payload = Some(Arc::new(move |payload: Value, _model: &Model| {
        let payload_slot = payload_slot.clone();
        Box::pin(async move {
            *payload_slot.lock().expect("lock") = Some(payload.clone());
            let mut next = payload.as_object().cloned().expect("payload object");
            // The upstream callback spreads the camelCase payload and adds
            // camelCase extras, which the wire remap must snake_case.
            next.insert("topP".to_owned(), json!(0.9));
            next.insert("randomSeed".to_owned(), json!(42));
            next.insert(
                "responseFormat".to_owned(),
                json!({
                    "type": "json_schema",
                    "jsonSchema": {
                        "name": "result",
                        "schemaDefinition": {
                            "type": "object",
                            "properties": {"maxTokens": {"type": "number"}},
                        },
                    },
                }),
            );
            next.insert("presencePenalty".to_owned(), json!(0.1));
            next.insert("frequencyPenalty".to_owned(), json!(0.2));
            next.insert("parallelToolCalls".to_owned(), json!(true));
            next.insert("safePrompt".to_owned(), json!(true));
            Some(Value::Object(next))
        })
    }));
    let response_slot = captured_response.clone();
    opts.stream.on_response = Some(Arc::new(
        move |response: rpi_ai::types::ProviderResponse, _model: &Model| {
            let response_slot = response_slot.clone();
            Box::pin(async move {
                *response_slot.lock().expect("lock") = Some((response.status, response.headers));
            })
        },
    ));

    let events = collect(stream_mistral(&m, &ctx, opts)).await;

    let Some(StreamEvent::Done { reason, message }) = events.last() else {
        panic!("expected done event, got {events:?}");
    };
    assert_eq!(*reason, rpi_ai::types::DoneReason::Stop);
    assert_eq!(message.stop_reason, StopReason::Stop);

    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer secret"));
    assert_eq!(request.header("accept"), Some("text/event-stream"));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("x-affinity"), Some("session-1"));
    assert_eq!(request.header("x-custom"), Some("value"));

    // `onPayload` observes the camelCase payload (9dd90a497).
    let callback_payload = captured_payload
        .lock()
        .expect("lock")
        .clone()
        .expect("payload");
    assert_eq!(callback_payload["maxTokens"], json!(123));
    assert_eq!(callback_payload["promptMode"], json!("reasoning"));
    assert_eq!(callback_payload["promptCacheKey"], json!("session-1"));

    let (status, headers) = captured_response
        .lock()
        .expect("lock")
        .clone()
        .expect("on_response");
    assert_eq!(status, 200);
    assert_eq!(
        headers.get("content-type").map(String::as_str),
        Some("text/event-stream")
    );
    assert_eq!(
        headers.get("x-request-id").map(String::as_str),
        Some("request-1")
    );

    // The wire body is snake_case (`toMistralWirePayload`), camelCase gone.
    let wire = request.body_json();
    assert_eq!(wire["max_tokens"], json!(123));
    assert_eq!(wire["prompt_mode"], json!("reasoning"));
    assert_eq!(wire["reasoning_effort"], json!("high"));
    assert_eq!(
        wire["tool_choice"],
        json!({"type": "function", "function": {"name": "lookup"}})
    );
    assert_eq!(wire["prompt_cache_key"], json!("session-1"));
    assert_eq!(wire["top_p"], json!(0.9));
    assert_eq!(wire["random_seed"], json!(42));
    assert_eq!(wire["presence_penalty"], json!(0.1));
    assert_eq!(wire["frequency_penalty"], json!(0.2));
    assert_eq!(wire["parallel_tool_calls"], json!(true));
    assert_eq!(wire["safe_prompt"], json!(true));
    assert_eq!(
        wire["response_format"],
        json!({
            "type": "json_schema",
            "json_schema": {
                "name": "result",
                "schema": {
                    "type": "object",
                    "properties": {"maxTokens": {"type": "number"}},
                },
            },
        })
    );
    assert!(wire.get("maxTokens").is_none());
    assert!(wire.get("promptMode").is_none());
    assert!(wire.get("promptCacheKey").is_none());
    assert_eq!(
        wire["messages"],
        json!([
            {"role": "system", "content": "Be precise"},
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "describe"},
                    {"type": "image_url", "image_url": "data:image/png;base64,aGVsbG8="},
                ],
            },
        ])
    );
}

/// mistral-http-transport.test.ts: "serializes assistant thinking, tool
/// calls, and tool results for replay".
#[tokio::test]
async fn mistral_http_transport_serializes_assistant_thinking_tool_calls_and_tool_results_for_replay()
 {
    let (base_url, mut captured) = serve_responses(vec![ScriptedResponse::Full {
        status: 200,
        headers: Vec::new(),
        body: TERMINAL_SSE,
    }])
    .await;
    let m = model(
        "mistral-large-latest",
        &base_url,
        json!({"input": ["text", "image"]}),
    );
    let history: Vec<Message> = serde_json::from_value(json!([
        {
            "role": "assistant", "api": "mistral-conversations", "provider": "mistral",
            "model": "mistral-large-latest", "timestamp": 1, "stopReason": "toolUse",
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                      "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "content": [
                {"type": "thinking", "thinking": "reason"},
                {"type": "text", "text": "answer"},
                {"type": "toolCall", "id": "abc123456", "name": "lookup", "arguments": {"query": "pi"}},
            ]
        },
        {
            "role": "toolResult", "toolCallId": "abc123456", "toolName": "lookup",
            "content": [
                {"type": "text", "text": "found"},
                {"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"},
            ],
            "isError": false, "timestamp": 2
        }
    ]))
    .expect("history");

    let events = collect(stream_mistral(&m, &context(history), {
        let mut opts = MistralOptions::default();
        opts.stream.request.api_key = Some("test".to_owned());
        opts
    }))
    .await;

    let Some(StreamEvent::Done { message, .. }) = events.last() else {
        panic!("expected done event, got {events:?}");
    };
    assert_eq!(message.stop_reason, StopReason::Stop);

    let request = captured.recv().await.expect("request captured");
    let wire = request.body_json();
    assert_eq!(
        wire["messages"],
        json!([
            {
                "role": "assistant",
                "prefix": false,
                "content": [
                    {"type": "thinking", "thinking": [{"type": "text", "text": "reason"}]},
                    {"type": "text", "text": "answer"},
                ],
                "tool_calls": [
                    {
                        "id": "abc123456",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{\"query\":\"pi\"}"},
                        "index": 0,
                    },
                ],
            },
            {
                "role": "tool",
                "tool_call_id": "abc123456",
                "name": "lookup",
                "content": [
                    {"type": "text", "text": "found"},
                    {"type": "image_url", "image_url": "data:image/png;base64,aGVsbG8="},
                ],
            },
        ])
    );
}

/// mistral-http-transport.test.ts: "parses native thinking, text, tool
/// calls, and cached-token usage".
#[tokio::test]
async fn mistral_http_transport_parses_native_thinking_text_tool_calls_and_cached_token_usage() {
    const NATIVE_SSE: &str = concat!(
        "data: {\"id\":\"response-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"delta\":{\"content\":[{\"type\":\"thinking\",\"thinking\":[{\"type\":\"text\",\"text\":\"reason\"}]}]}}]}\r\n",
        "\r\n",
        "data: {\"id\":\"response-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"delta\":{\"content\":[{\"type\":\"text\",\"text\":\"answer\"}]}}]}\r\n",
        "\r\n",
        "data: {\"id\":\"response-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"delta\":{\"tool_calls\":[{\"id\":\"abc123456\",\"index\":0,\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"query\\\":\"}}]}}]}\r\n",
        "\r\n",
        "data: {\"id\":\"response-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"\",\"arguments\":\"\\\"pi\\\"}\"}}]}}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":4,\"total_tokens\":14,\"prompt_tokens_details\":{\"cached_tokens\":3}}}\r\n",
        "\r\n",
        "data: [DONE]\r\n",
        "\r\n",
    );
    let (base_url, mut captured) = serve(vec![(200, NATIVE_SSE)]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));
    let events = collect(MistralConversations.stream(
        &m,
        &context(vec![user_text("hello")]),
        Some(options()),
    ))
    .await;

    captured.recv().await.expect("request captured");
    let Some(StreamEvent::Done { reason, message }) = events.last() else {
        panic!("expected done event, got {events:?}");
    };
    assert_eq!(*reason, rpi_ai::types::DoneReason::ToolUse);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(message.response_id.as_deref(), Some("response-1"));
    assert_eq!(message.content.len(), 3);
    match &message.content[0] {
        rpi_ai::types::AssistantContent::Thinking(thinking) => {
            assert_eq!(thinking.thinking, "reason")
        }
        other => panic!("expected thinking block, got {other:?}"),
    }
    match &message.content[1] {
        rpi_ai::types::AssistantContent::Text(text) => assert_eq!(text.text, "answer"),
        other => panic!("expected text block, got {other:?}"),
    }
    match &message.content[2] {
        rpi_ai::types::AssistantContent::ToolCall(call) => {
            assert_eq!(call.id, "abc123456");
            assert_eq!(call.name, "lookup");
            assert_eq!(
                serde_json::Value::Object(call.arguments.clone()),
                json!({"query": "pi"})
            );
        }
        other => panic!("expected tool call block, got {other:?}"),
    }
    assert_eq!(message.usage.input, 7);
    assert_eq!(message.usage.output, 4);
    assert_eq!(message.usage.cache_read, 3);
    assert_eq!(message.usage.cache_write, 0);
    assert_eq!(message.usage.total_tokens, 14);
}

/// mistral-http-transport.test.ts: "parses SSE and UTF-8 sequences split
/// across transport chunks".
#[tokio::test]
async fn mistral_http_transport_parses_sse_and_utf_8_sequences_split_across_transport_chunks() {
    const BYTEWISE_SSE: &str = concat!(
        "data: {\"id\":\"response-bytewise\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"delta\":{\"content\":\"héllo 🌍\"}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\r\n",
        "\r\n",
        "data: [DONE]\r\n",
        "\r\n",
    );
    let (base_url, mut captured) =
        serve_responses(vec![ScriptedResponse::Dribble(BYTEWISE_SSE)]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));
    let events = collect(MistralConversations.stream(
        &m,
        &context(vec![user_text("hello")]),
        Some(options()),
    ))
    .await;

    captured.recv().await.expect("request captured");
    let Some(StreamEvent::Done { message, .. }) = events.last() else {
        panic!("expected done event, got {events:?}");
    };
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.content.len(), 1);
    match &message.content[0] {
        rpi_ai::types::AssistantContent::Text(text) => assert_eq!(text.text, "héllo 🌍"),
        other => panic!("expected text block, got {other:?}"),
    }
}

/// Tool-key edge case pinned to the upstream map semantics (6c87d9a02 /
/// #8387): the key is `index ?? callId`, so a chunk keyed by `CallId` (id
/// present, index absent) and a later indexed chunk land in **separate**
/// blocks — the upstream map does not merge them, and neither do we.
#[tokio::test]
async fn mistral_id_keyed_chunk_then_indexed_chunk_open_separate_blocks() {
    const SPLIT_SSE: &str = concat!(
        "data: {\"id\":\"response-split\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"delta\":{\"tool_calls\":[{\"id\":\"idonly\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{}\"}}]}}]}\r\n",
        "\r\n",
        "data: {\"id\":\"response-split\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"lookup\",\"arguments\":\"{}\"}}]}}]}\r\n",
        "\r\n",
        "data: [DONE]\r\n",
        "\r\n",
    );
    let (base_url, mut captured) = serve(vec![(200, SPLIT_SSE)]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));
    let events = collect(MistralConversations.stream(
        &m,
        &context(vec![user_text("hello")]),
        Some(options()),
    ))
    .await;

    captured.recv().await.expect("request captured");
    let starts = events
        .iter()
        .filter(|event| matches!(event, StreamEvent::ToolCallStart { .. }))
        .count();
    assert_eq!(starts, 2, "separate blocks, not merged: {events:?}");
    let Some(StreamEvent::Done { message, .. }) = events.last() else {
        panic!("expected done event, got {events:?}");
    };
    let tool_calls: Vec<&rpi_ai::types::ToolCall> = message
        .content
        .iter()
        .map(|block| match block {
            rpi_ai::types::AssistantContent::ToolCall(call) => call,
            other => panic!("expected tool call block, got {other:?}"),
        })
        .collect();
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0].id, "idonly");
    // Continuation chunk without an id derives `toolcall:{index ?? 0}`.
    assert_ne!(tool_calls[1].id, "idonly");
}

/// mistral-http-transport.test.ts: "honors case-insensitive header overrides
/// and explicit affinity suppression".
#[tokio::test]
async fn mistral_http_transport_honors_case_insensitive_header_overrides_and_explicit_affinity_suppression()
 {
    let (base_url, mut captured) = serve_responses(vec![ScriptedResponse::Full {
        status: 200,
        headers: Vec::new(),
        body: TERMINAL_SSE,
    }])
    .await;
    let m = model(
        "mistral-large-latest",
        &base_url,
        json!({"headers": {"Authorization": "Bearer model-key", "X-Affinity": "model-affinity"}}),
    );
    let mut opts = options();
    opts.request.api_key = Some("request-key".to_owned());
    opts.session_id = Some("automatic-affinity".to_owned());
    // `null` overrides delete the header and count as explicit x-affinity.
    opts.headers = Some(ProviderHeaders::from([
        ("authorization".to_owned(), None),
        ("x-affinity".to_owned(), None),
    ]));
    let events =
        collect(MistralConversations.stream(&m, &context(vec![user_text("hello")]), Some(opts)))
            .await;

    let request = captured.recv().await.expect("request captured");
    assert!(request.header("authorization").is_none());
    assert!(request.header("x-affinity").is_none());
    assert_eq!(event_kinds(&events).last(), Some(&"done"));
}

/// mistral-http-transport.test.ts: "aborts while waiting for an SSE chunk".
#[tokio::test]
async fn mistral_http_transport_aborts_while_waiting_for_an_sse_chunk() {
    let (base_url, _captured) = serve_responses(vec![ScriptedResponse::Hang]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));
    let token = CancellationToken::new();
    let mut opts = options();
    opts.signal = Some(token.clone());
    let handle = tokio::spawn(collect(MistralConversations.stream(
        &m,
        &context(vec![user_text("hello")]),
        Some(opts),
    )));
    // Abort after the request is in flight, while the body never arrives.
    tokio::time::sleep(Duration::from_millis(50)).await;
    token.cancel();
    let events = handle.await.expect("collect task");

    // Upstream asserts only the aborted stop reason; whether `start` was
    // already emitted depends on how far the request got before the cancel.
    let Some(StreamEvent::Error { reason, error }) = events.last() else {
        panic!("expected error event, got {events:?}");
    };
    assert_eq!(*reason, rpi_ai::types::ErrorReason::Aborted);
    assert_eq!(error.stop_reason, StopReason::Aborted);
}

/// mistral-http-transport.test.ts: "applies the request timeout while
/// waiting for an SSE chunk".
#[tokio::test]
async fn mistral_http_transport_applies_the_request_timeout_while_waiting_for_an_sse_chunk() {
    let (base_url, _captured) = serve_responses(vec![ScriptedResponse::Hang]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));
    let mut opts = options();
    opts.timeout_ms = Some(5);
    let events =
        collect(MistralConversations.stream(&m, &context(vec![user_text("hello")]), Some(opts)))
            .await;

    // `start` may or may not precede the error depending on whether the
    // response headers beat the 5ms timeout.
    let Some(StreamEvent::Error { reason, error }) = events.last() else {
        panic!("expected error event, got {events:?}");
    };
    assert_eq!(*reason, rpi_ai::types::ErrorReason::Error);
    assert_eq!(error.stop_reason, StopReason::Error);
    // Upstream surfaces the Node `TimeoutError` DOMException text
    // ("The operation was aborted due to timeout", matched by `/timeout/i`).
    assert_eq!(
        error.error_message.as_deref(),
        Some("The operation was aborted due to timeout")
    );
}

/// mistral-http-transport.test.ts: "preserves HTTP status and response
/// bodies in errors".
#[tokio::test]
async fn mistral_http_transport_preserves_http_status_and_response_bodies_in_errors() {
    let error_body = "{\"message\":\"blocked by gateway\"}";
    let (base_url, mut captured) = serve(vec![(403, error_body)]).await;
    let m = model("mistral-large-latest", &base_url, json!({}));
    let events = collect(MistralConversations.stream(
        &m,
        &context(vec![user_text("hello")]),
        Some(options()),
    ))
    .await;

    captured.recv().await.expect("request captured");
    assert_eq!(event_kinds(&events), vec!["error"]);
    let StreamEvent::Error { error, .. } = &events[0] else {
        panic!("expected error event");
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(
        error.error_message.as_deref(),
        Some("Mistral API error (403): {\"message\":\"blocked by gateway\"}")
    );
}
