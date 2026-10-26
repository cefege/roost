//! Contract tests for the pi-messages adapter, porting the intent of
//! `packages/ai/test/pi-messages.test.ts` @ pi 0.82.1 (2efa728): drive
//! `stream()` over a scripted loopback HTTP server and assert both sides of
//! the contract — the request shape (method / path / headers / body JSON) and
//! the emitted `StreamEvent` sequence plus terminal message. No real network.
//!
//! Test names mirror the upstream vitest cases (snake_case). The upstream
//! "api registration" describe block has no rpi counterpart yet (no builtin
//! provider registry); `test_is_a_known_api_usable_on_models` covers the
//! `ApiKind` half.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use rpi_ai::api::pi_messages::{PiMessages, PiMessagesOptions, stream};
use rpi_ai::models::ProviderStreams;
use rpi_ai::types::{
    ApiKind, AssistantContent, Context, Model, ProviderResponse, StopReason, StreamEvent,
    StreamOptions,
};
use rpi_ai::utils::event_stream::AssistantMessageEventStream;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Scripted HTTP server (same approach as contract_adapters.rs)
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

/// One scripted response: status, extra headers, body.
struct ScriptResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl ScriptResponse {
    fn sse(events: Vec<Value>) -> Self {
        let mut body = String::new();
        for event in events {
            body.push_str(&format!(
                "data: {}\n\n",
                serde_json::to_string(&event).expect("json")
            ));
        }
        Self {
            status: 200,
            headers: Vec::new(),
            body,
        }
    }

    fn error(status: u16, body: String) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body,
        }
    }

    fn with_headers(mut self, headers: &[(&str, &str)]) -> Self {
        self.headers = headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        self
    }
}

/// Serves the scripted responses, one per connection, on a loopback port.
/// Returns the base URL and a channel of captured requests.
async fn serve(script: Vec<ScriptResponse>) -> (String, mpsc::Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel(script.len().max(1));
    tokio::spawn(async move {
        for response_script in script {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let request = read_request(&mut socket).await;
            tx.send(request).await.expect("send captured request");
            let reason = match response_script.status {
                200 => "OK",
                401 => "Unauthorized",
                _ => "Status",
            };
            let content_type = if response_script.status == 200 {
                "text/event-stream"
            } else {
                "application/json"
            };
            let mut response = format!(
                "HTTP/1.1 {} {reason}\r\ncontent-type: {content_type}\r\n",
                response_script.status
            );
            for (name, value) in &response_script.headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str(&format!(
                "content-length: {}\r\nconnection: close\r\n\r\n{}",
                response_script.body.len(),
                response_script.body
            ));
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
    });
    (format!("http://{addr}"), rx)
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
// Test helpers (mirroring the upstream fixtures)
// ---------------------------------------------------------------------------

/// Upstream `createModel`: the Radius "auto" model on the given base URL.
fn create_model(base_url: &str) -> Model {
    serde_json::from_value(json!({
        "id": "auto", "name": "Radius Auto", "api": "pi-messages", "provider": "radius",
        "baseUrl": base_url, "reasoning": false, "input": ["text"],
        "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.1, "cacheWrite": 0.2},
        "contextWindow": 128000, "maxTokens": 16384
    }))
    .expect("model")
}

/// Upstream `context` fixture (fixed timestamp for determinism).
fn context() -> rpi_ai::types::TranscriptContext {
    rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            serde_json::from_value(json!({"role": "user", "content": "Hello", "timestamp": 0}))
                .expect("user message"),
        ],
        tools: None,
    })
}

/// Upstream `usage` fixture.
fn usage_json() -> Value {
    json!({
        "input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 15,
        "cost": {"input": 0.1, "output": 0.2, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.3}
    })
}

fn assert_usage_fixture(message: &rpi_ai::types::AssistantMessage) {
    assert_eq!(message.usage.input, 10);
    assert_eq!(message.usage.output, 5);
    assert_eq!(message.usage.cache_read, 0);
    assert_eq!(message.usage.cache_write, 0);
    assert_eq!(message.usage.total_tokens, 15);
    assert_eq!(message.usage.cost.total, 0.3);
}

async fn collect(stream: AssistantMessageEventStream) -> Vec<StreamEvent> {
    tokio::time::timeout(Duration::from_secs(10), stream.collect())
        .await
        .expect("stream completes within 10s")
}

async fn result(stream: &AssistantMessageEventStream) -> rpi_ai::types::AssistantMessage {
    tokio::time::timeout(Duration::from_secs(10), stream.result())
        .await
        .expect("result resolves within 10s")
        .expect("terminal message")
}

fn api_key_options() -> StreamOptions {
    StreamOptions {
        request: rpi_ai::ProviderRequestOptions {
            api_key: Some("test-key".to_owned()),
            ..Default::default()
        },
        ..StreamOptions::default()
    }
}

// ---------------------------------------------------------------------------
// pi-messages describe block
// ---------------------------------------------------------------------------

/// Upstream: "streams text and tool calls and resolves the terminal message".
#[tokio::test]
async fn test_streams_text_and_tool_calls_and_resolves_the_terminal_message() {
    let (base_url, mut captured) = serve(vec![ScriptResponse::sse(vec![
        json!({"type": "start"}),
        json!({"type": "text_start", "contentIndex": 0}),
        json!({"type": "text_delta", "contentIndex": 0, "delta": "Hel"}),
        json!({"type": "text_delta", "contentIndex": 0, "delta": "lo"}),
        json!({"type": "text_end", "contentIndex": 0, "content": "Hello"}),
        json!({"type": "toolcall_start", "contentIndex": 1, "id": "call_1", "toolName": "read"}),
        json!({"type": "toolcall_delta", "contentIndex": 1, "delta": "{\"path\":"}),
        json!({"type": "toolcall_delta", "contentIndex": 1, "delta": "\"a.txt\"}"}),
        json!({
            "type": "toolcall_end", "contentIndex": 1,
            "toolCall": {"type": "toolCall", "id": "call_1", "name": "read", "arguments": {"path": "a.txt"}}
        }),
        json!({"type": "done", "reason": "toolUse", "usage": usage_json(), "responseId": "resp_1"}),
    ])])
    .await;
    let model = create_model(&format!("{base_url}/v1"));

    let event_stream = stream(
        &model,
        &context(),
        PiMessagesOptions {
            stream: StreamOptions {
                session_id: Some("session-1".to_owned()),
                max_tokens: Some(100),
                request: rpi_ai::ProviderRequestOptions {
                    api_key: Some("test-key".to_owned()),
                    headers: Some(
                        [("x-custom".to_owned(), Some("1".to_owned()))]
                            .into_iter()
                            .collect(),
                    ),
                    ..Default::default()
                },
                ..StreamOptions::default()
            },
            tool_choice: Some(json!("auto")),
            ..PiMessagesOptions::default()
        },
    );
    let result_future = event_stream.result();
    let events = collect(event_stream).await;
    let message = result_future.await.expect("message");

    let partial_stop_reasons: Vec<StopReason> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Start { partial }
            | StreamEvent::TextStart { partial, .. }
            | StreamEvent::TextDelta { partial, .. }
            | StreamEvent::TextEnd { partial, .. }
            | StreamEvent::ThinkingStart { partial, .. }
            | StreamEvent::ThinkingDelta { partial, .. }
            | StreamEvent::ThinkingEnd { partial, .. }
            | StreamEvent::ToolCallStart { partial, .. }
            | StreamEvent::ToolCallDelta { partial, .. }
            | StreamEvent::ToolCallEnd { partial, .. } => Some(partial.stop_reason),
            StreamEvent::Done { .. } | StreamEvent::Error { .. } => None,
        })
        .collect();
    assert_eq!(partial_stop_reasons.first(), Some(&StopReason::Pending));

    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_usage_fixture(&message);
    assert_eq!(message.response_id.as_deref(), Some("resp_1"));
    assert_eq!(message.model, "auto");
    assert_eq!(message.provider, "radius");
    assert_eq!(message.content.len(), 2);
    let AssistantContent::Text(text) = &message.content[0] else {
        panic!("expected text block at index 0");
    };
    assert_eq!(text.text, "Hello");
    assert_eq!(text.text_signature, None);
    let AssistantContent::ToolCall(tool_call) = &message.content[1] else {
        panic!("expected tool call block at index 1");
    };
    assert_eq!(tool_call.id, "call_1");
    assert_eq!(tool_call.name, "read");
    assert_eq!(tool_call.arguments["path"], json!("a.txt"));

    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { .. }))
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, StreamEvent::ToolCallEnd { .. }))
            .count(),
        1
    );

    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/messages");
    assert_eq!(request.header("authorization"), Some("Bearer test-key"));
    assert_eq!(request.header("x-custom"), Some("1"));
    assert_eq!(
        request.body_json(),
        json!({
            "model": "auto",
            "context": {"messages": [{"role": "user", "content": "Hello", "timestamp": 0}]},
            "options": {"maxTokens": 100, "sessionId": "session-1", "toolChoice": "auto"}
        })
    );
}

/// Thinking anchor (design §3.3 / requirements §5.2): thinking blocks stream
/// with signature and redacted flag (upstream test file has no thinking case;
/// the intent is the converter's `thinking_*` handling).
#[tokio::test]
async fn test_streams_thinking_blocks_with_signature_and_redacted_flag() {
    let (base_url, _captured) = serve(vec![ScriptResponse::sse(vec![
        json!({"type": "start"}),
        json!({"type": "thinking_start", "contentIndex": 0}),
        json!({"type": "thinking_delta", "contentIndex": 0, "delta": "think"}),
        json!({"type": "thinking_delta", "contentIndex": 0, "delta": "ing"}),
        json!({"type": "thinking_end", "contentIndex": 0, "content": "thinking", "contentSignature": "sig_1"}),
        json!({"type": "thinking_start", "contentIndex": 1}),
        json!({"type": "thinking_end", "contentIndex": 1, "content": "", "contentSignature": "enc_1", "redacted": true}),
        json!({"type": "done", "reason": "stop", "usage": usage_json()}),
    ])])
    .await;
    let model = create_model(&format!("{base_url}/v1"));

    let event_stream = stream(
        &model,
        &context(),
        PiMessagesOptions {
            stream: api_key_options(),
            ..PiMessagesOptions::default()
        },
    );
    let message = result(&event_stream).await;
    let events = collect(event_stream).await;

    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.content.len(), 2);
    let AssistantContent::Thinking(thinking) = &message.content[0] else {
        panic!("expected thinking block at index 0");
    };
    assert_eq!(thinking.thinking, "thinking");
    assert_eq!(thinking.thinking_signature.as_deref(), Some("sig_1"));
    assert_eq!(thinking.redacted, None);
    let AssistantContent::Thinking(redacted) = &message.content[1] else {
        panic!("expected redacted thinking block at index 1");
    };
    assert_eq!(redacted.redacted, Some(true));
    assert_eq!(redacted.thinking_signature.as_deref(), Some("enc_1"));

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, StreamEvent::ThinkingDelta { .. }))
            .count(),
        2
    );
}

/// Upstream: "appends debug=1 and reports response headers via onResponse".
/// Upstream smuggles `debug` through `streamSimple`; rpi's
/// `SimpleStreamOptions` cannot carry it (D-021), so `stream` is called
/// directly with `PiMessagesOptions::debug`.
#[tokio::test]
async fn test_appends_debug_1_and_reports_response_headers_via_on_response() {
    let (base_url, mut captured) = serve(vec![
        ScriptResponse::sse(vec![
            json!({"type": "done", "reason": "stop", "usage": usage_json()}),
        ])
        .with_headers(&[("x-pi-gateway-upstream-provider", "anthropic")]),
    ])
    .await;
    let model = create_model(&format!("{base_url}/v1"));

    let observed: Arc<Mutex<Option<ProviderResponse>>> = Arc::new(Mutex::new(None));
    let observed_clone = observed.clone();
    let event_stream = stream(
        &model,
        &context(),
        PiMessagesOptions {
            stream: StreamOptions {
                request: rpi_ai::ProviderRequestOptions {
                    on_response: Some(Arc::new(move |response, _model| {
                        let observed = observed_clone.clone();
                        Box::pin(async move {
                            *observed.lock().unwrap_or_else(|e| e.into_inner()) = Some(response);
                        })
                    })),
                    ..api_key_options().request
                },
                ..api_key_options()
            },
            debug: Some(true),
            ..PiMessagesOptions::default()
        },
    );
    let message = result(&event_stream).await;
    collect(event_stream).await;

    assert_eq!(message.stop_reason, StopReason::Stop);
    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.path, "/v1/messages?debug=1");
    let observed = observed.lock().unwrap_or_else(|e| e.into_inner());
    let observed = observed.as_ref().expect("on_response invoked");
    assert_eq!(
        observed
            .headers
            .get("x-pi-gateway-upstream-provider")
            .map(String::as_str),
        Some("anthropic")
    );
}

/// Upstream: "surfaces backend error responses with diagnostics".
#[tokio::test]
async fn test_surfaces_backend_error_responses_with_diagnostics() {
    let (base_url, _captured) = serve(vec![ScriptResponse::error(
        401,
        serde_json::to_string(
            &json!({"error": {"message": "Token expired", "code": "unauthorized"}}),
        )
        .expect("json"),
    )])
    .await;
    let model = create_model(&format!("{base_url}/v1"));

    let event_stream = stream(
        &model,
        &context(),
        PiMessagesOptions {
            stream: StreamOptions {
                request: rpi_ai::ProviderRequestOptions {
                    api_key: Some("stale".to_owned()),
                    ..Default::default()
                },
                ..StreamOptions::default()
            },
            ..PiMessagesOptions::default()
        },
    );
    let message = result(&event_stream).await;
    collect(event_stream).await;

    assert_eq!(message.stop_reason, StopReason::Error);
    let error_message = message.error_message.as_deref().expect("error message");
    assert!(error_message.contains("401"), "{error_message}");
    assert!(error_message.contains("Token expired"), "{error_message}");
    assert!(error_message.contains("unauthorized"), "{error_message}");
    let diagnostics = message.diagnostics.as_ref().expect("diagnostics");
    assert_eq!(diagnostics[0].kind, "pi_messages_response_failure");
    let details = diagnostics[0].details.as_ref().expect("details");
    assert_eq!(details.get("status"), Some(&json!(401)));
}

/// Rewrite-diagnostic anchor (requirements §5.2, design §3.3): a `done`/`error`
/// event carrying `rewrite` appends a `pi_messages_rewrite` diagnostic.
#[tokio::test]
async fn test_appends_rewrite_diagnostic_from_done_event() {
    let rewrite = json!({
        "policyId": "gateway-policy", "policyVersion": 3, "changed": true,
        "tokenCountChange": -12, "messageCountChange": 1, "systemPromptChanged": false
    });
    let (base_url, _captured) = serve(vec![ScriptResponse::sse(vec![
        json!({"type": "start"}),
        json!({"type": "text_start", "contentIndex": 0}),
        json!({"type": "text_delta", "contentIndex": 0, "delta": "hi"}),
        json!({"type": "text_end", "contentIndex": 0, "content": "hi"}),
        json!({"type": "done", "reason": "stop", "usage": usage_json(), "rewrite": rewrite}),
    ])])
    .await;
    let model = create_model(&format!("{base_url}/v1"));

    let event_stream = stream(
        &model,
        &context(),
        PiMessagesOptions {
            stream: api_key_options(),
            ..PiMessagesOptions::default()
        },
    );
    let message = result(&event_stream).await;
    collect(event_stream).await;

    assert_eq!(message.stop_reason, StopReason::Stop);
    let diagnostics = message.diagnostics.as_ref().expect("diagnostics");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, "pi_messages_rewrite");
    assert!(diagnostics[0].error.is_none());
    let details = diagnostics[0].details.as_ref().expect("details");
    assert_eq!(details.get("policyId"), Some(&json!("gateway-policy")));
    assert_eq!(details.get("policyVersion"), Some(&json!(3)));
    assert_eq!(details.get("tokenCountChange"), Some(&json!(-12)));
}

/// Upstream: "propagates server-sent error events".
#[tokio::test]
async fn test_propagates_server_sent_error_events() {
    let (base_url, _captured) = serve(vec![ScriptResponse::sse(vec![
        json!({"type": "start"}),
        json!({"type": "error", "reason": "error", "usage": usage_json(), "errorMessage": "Upstream failed"}),
    ])])
    .await;
    let model = create_model(&format!("{base_url}/v1"));

    let event_stream = stream(
        &model,
        &context(),
        PiMessagesOptions {
            stream: api_key_options(),
            ..PiMessagesOptions::default()
        },
    );
    let message = result(&event_stream).await;
    let events = collect(event_stream).await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("Upstream failed"));
    assert_usage_fixture(&message);
    assert!(matches!(events.last(), Some(StreamEvent::Error { .. })));
}

/// Upstream: "errors when no API key is provided".
#[tokio::test]
async fn test_errors_when_no_api_key_is_provided() {
    let model = create_model("http://127.0.0.1:1/v1");

    let event_stream = stream(&model, &context(), PiMessagesOptions::default());
    let message = result(&event_stream).await;
    collect(event_stream).await;

    assert_eq!(message.stop_reason, StopReason::Error);
    let error_message = message.error_message.as_deref().expect("error message");
    assert!(
        error_message.contains("No API key provided"),
        "{error_message}"
    );
}

/// Upstream: "errors when the stream ends without a terminal event".
#[tokio::test]
async fn test_errors_when_the_stream_ends_without_a_terminal_event() {
    let (base_url, _captured) = serve(vec![ScriptResponse::sse(vec![
        json!({"type": "start"}),
        json!({"type": "text_start", "contentIndex": 0}),
        json!({"type": "text_delta", "contentIndex": 0, "delta": "partial"}),
    ])])
    .await;
    let model = create_model(&format!("{base_url}/v1"));

    let event_stream = stream(
        &model,
        &context(),
        PiMessagesOptions {
            stream: api_key_options(),
            ..PiMessagesOptions::default()
        },
    );
    let message = result(&event_stream).await;
    collect(event_stream).await;

    assert_eq!(message.stop_reason, StopReason::Error);
    let error_message = message.error_message.as_deref().expect("error message");
    assert!(
        error_message.contains("stream ended without a terminal event"),
        "{error_message}"
    );
}

// ---------------------------------------------------------------------------
// pi-messages api registration describe block
// ---------------------------------------------------------------------------

/// Upstream: "is a known api usable on models". The "is registered as a
/// builtin api provider" case has no rpi counterpart yet (no builtin provider
/// registry in rpi-ai); `PiMessages` implements `ProviderStreams`, which is
/// what a registry entry would wrap.
#[tokio::test]
async fn test_is_a_known_api_usable_on_models() {
    assert_eq!(ApiKind::RPI_MESSAGES, "rpi-messages");
    // Legacy alias from the pre-rename spelling still resolves.
    assert_eq!(ApiKind::from("pi-messages").as_str(), "rpi-messages");
    let streams: &dyn ProviderStreams = &PiMessages;
    let model = create_model("http://127.0.0.1:1/v1");
    // Dispatching through the trait yields the same missing-key error stream.
    let event_stream = streams.stream(&model, &context(), None);
    let message = result(&event_stream).await;
    collect(event_stream).await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(
        message
            .error_message
            .as_deref()
            .expect("error message")
            .contains("No API key provided")
    );
}
