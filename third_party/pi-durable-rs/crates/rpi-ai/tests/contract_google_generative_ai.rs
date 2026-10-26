//! Contract tests for the Google Generative AI adapter (T13 W1), mirroring
//! the upstream suites `google-shared-convert-tools.test.ts`,
//! `google-shared-gemini3-unsigned-tool-call.test.ts`,
//! `google-shared-image-tool-result-routing.test.ts`,
//! `google-shared-signed-empty-blocks.test.ts` (6138f5a07 @ 4181f66),
//! `google-shared-retry.test.ts` (b9d360a2c @ 4181f66),
//! `google-thinking-disable.test.ts` (E2E intent ported to wire-shape
//! assertions — upstream's live tests are env-gated) and
//! `google-thinking-signature.test.ts`, plus the requirements §5.2 anchors:
//! the thinking budget table, the usage mapping, thoughtSignature retention,
//! non-streaming function calls, and self-incrementing tool call ids.
//!
//! The SSE payloads use the Gemini API wire format (the shapes the pinned
//! `@google/genai` SDK feeds through `generateContentResponseFromMldev`).

use std::time::Duration;

use futures::StreamExt;
use rpi_ai::api::google_generative_ai::{GoogleGenerativeAi, stream_simple};
use rpi_ai::api::google_shared::{
    convert_messages, convert_tools, is_thinking_part, map_stop_reason, map_stop_reason_string,
    requires_tool_call_id, resolve_google_function_calling_mode, retain_thought_signature,
    supports_google_strict_tool_sampling,
};
use rpi_ai::models::ProviderStreams;
use rpi_ai::types::{
    ApiKind, Context, Message, Model, SimpleStreamOptions, StopReason, StreamEvent, StreamOptions,
    ThinkingBudgets, ThinkingLevel, Tool,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

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

/// Serves the scripted `(status, body)` responses, one per connection, on a
/// loopback port. Returns the base URL and a channel of captured requests.
async fn serve(script: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel(script.len().max(1));
    tokio::spawn(async move {
        for (status, body) in script {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let request = read_request(&mut socket).await;
            tx.send(request).await.expect("send captured request");
            let reason = match status {
                200 => "OK",
                400 => "Bad Request",
                429 => "Too Many Requests",
                _ => "Status",
            };
            let content_type = if status == 200 {
                "text/event-stream"
            } else {
                "application/json"
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
    });
    (format!("http://{addr}"), rx)
}

/// Scripted response for [`serve_with_response_headers`]: status, extra
/// response headers, body.
type ScriptedResponse = (u16, Vec<(&'static str, &'static str)>, &'static str);

/// `serve` variant whose responses carry extra headers (the retry-after
/// test needs a `retry-after` response header).
async fn serve_with_response_headers(
    script: Vec<ScriptedResponse>,
) -> (String, mpsc::Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel(script.len().max(1));
    tokio::spawn(async move {
        for (status, extra_headers, body) in script {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let request = read_request(&mut socket).await;
            tx.send(request).await.expect("send captured request");
            let reason = match status {
                200 => "OK",
                400 => "Bad Request",
                429 => "Too Many Requests",
                _ => "Status",
            };
            let content_type = if status == 200 {
                "text/event-stream"
            } else {
                "application/json"
            };
            let mut response =
                format!("HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\n");
            for (name, value) in extra_headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str(&format!(
                "content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
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
// Test helpers
// ---------------------------------------------------------------------------

fn model_with_id(id: &str, base_url: &str, extra: Value) -> Model {
    let mut value = json!({
        "id": id, "name": id, "api": ApiKind::GOOGLE_GENERATIVE_AI, "provider": "google",
        "baseUrl": base_url, "reasoning": true, "input": ["text"],
        "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.5, "cacheWrite": 1.0},
        "contextWindow": 128000, "maxTokens": 8192
    });
    value
        .as_object_mut()
        .expect("object")
        .extend(extra.as_object().cloned().unwrap_or_default());
    serde_json::from_value(value).expect("model")
}

fn model(base_url: &str) -> Model {
    model_with_id("test-model", base_url, json!({}))
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

fn simple_options(
    reasoning: Option<ThinkingLevel>,
    budgets: Option<ThinkingBudgets>,
) -> SimpleStreamOptions {
    SimpleStreamOptions {
        stream: options(),
        reasoning,
        thinking_budgets: budgets,
        tool_choice: None,
    }
}

fn make_tool(parameters: Value) -> Tool {
    serde_json::from_value(json!({
        "name": "test_tool", "description": "A test tool", "parameters": parameters
    }))
    .expect("tool")
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
// Recorded SSE streams (Gemini API wire format)
// ---------------------------------------------------------------------------

const GOOGLE_TEXT_SSE: &str = concat!(
    "data: {\"responseId\":\"resp-1\",\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hello\"}]}}]}\n",
    "\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\" world\"}]}}],\"usageMetadata\":{\"promptTokenCount\":10,\"cachedContentTokenCount\":4,\"candidatesTokenCount\":5,\"thoughtsTokenCount\":2,\"totalTokenCount\":21}}\n",
    "\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[]},\"finishReason\":\"STOP\"}]}\n",
    "\n",
);

const GOOGLE_THINKING_SSE: &str = concat!(
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"thought\":true,\"text\":\"thinking...\",\"thoughtSignature\":\"QUJDREVGRw==\"}]}}]}\n",
    "\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"thought\":true,\"text\":\" more\"}]}}]}\n",
    "\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Answer\"}]}}]}\n",
    "\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":8,\"candidatesTokenCount\":3,\"thoughtsTokenCount\":12,\"totalTokenCount\":23}}\n",
    "\n",
);

const GOOGLE_TOOL_CALL_SSE: &str = concat!(
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Let me check.\"}]}}]}\n",
    "\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"bash\",\"args\":{\"command\":\"ls\"}},\"thoughtSignature\":\"QUJDREVGRw==\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":5,\"totalTokenCount\":15}}\n",
    "\n",
);

/// Ends without any finish reason.
const GOOGLE_NO_FINISH_SSE: &str = concat!(
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"dangling\"}]}}]}\n",
    "\n",
);

// ---------------------------------------------------------------------------
// Normal flow contract
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_google_generative_ai_contract() {
    let (base_url, mut captured) = serve(vec![(200, GOOGLE_TEXT_SSE)]).await;
    let m = model(&base_url);
    let events =
        collect(GoogleGenerativeAi.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;

    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.method, "POST");
    // SDK-derived path: {baseUrl}/models/{model}:streamGenerateContent?alt=sse.
    assert_eq!(
        request.path,
        "/models/test-model:streamGenerateContent?alt=sse"
    );
    assert_eq!(request.header("x-goog-api-key"), Some("test-key"));
    let body = request.body_json();
    // The model id goes to the URL, not the body.
    assert!(body.get("model").is_none());
    assert_eq!(body["contents"][0]["role"], json!("user"));
    assert_eq!(body["contents"][0]["parts"][0]["text"], json!("hi"));
    // No generationConfig without temperature/maxTokens/thinking.
    assert!(body.get("generationConfig").is_none());

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
    // 23cb385b6: the raw provider finish reason is preserved.
    assert_eq!(message.raw_stop_reason.as_deref(), Some("STOP"));
    assert_eq!(message.response_id.as_deref(), Some("resp-1"));
    // Usage mapping anchor (requirements §5.2):
    // input = promptTokenCount − cachedContentTokenCount,
    // output = candidatesTokenCount + thoughtsTokenCount,
    // cacheRead = cachedContentTokenCount, cacheWrite = 0,
    // reasoning = thoughtsTokenCount, totalTokens = totalTokenCount.
    assert_eq!(message.usage.input, 6);
    assert_eq!(message.usage.output, 7);
    assert_eq!(message.usage.cache_read, 4);
    assert_eq!(message.usage.cache_write, 0);
    assert_eq!(message.usage.reasoning, Some(2));
    assert_eq!(message.usage.total_tokens, 21);
}

#[tokio::test]
async fn test_google_generative_ai_system_prompt_and_generation_config() {
    let (base_url, mut captured) = serve(vec![(200, GOOGLE_TEXT_SSE)]).await;
    let m = model(&base_url);
    let ctx = rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: Some("Be brief.".to_owned()),
        messages: vec![user_text("hi")],
        tools: None,
    });
    let mut opts = options();
    opts.temperature = Some(0.5);
    opts.max_tokens = Some(256);
    let events = collect(GoogleGenerativeAi.stream(&m, &ctx, Some(opts))).await;
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));

    let request = captured.recv().await.expect("request captured");
    let body = request.body_json();
    // SDK tContent(string) → {role: "user", parts: [{text}]}.
    assert_eq!(
        body["systemInstruction"],
        json!({"parts": [{"text": "Be brief."}], "role": "user"})
    );
    assert_eq!(
        body["generationConfig"],
        json!({"temperature": 0.5, "maxOutputTokens": 256})
    );
}

// ---------------------------------------------------------------------------
// Thinking stream + thoughtSignature preservation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_google_thinking_stream_retains_thought_signature() {
    let (base_url, _captured) = serve(vec![(200, GOOGLE_THINKING_SSE)]).await;
    let m = model(&base_url);
    let events =
        collect(GoogleGenerativeAi.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;

    assert_eq!(
        event_kinds(&events),
        vec![
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_delta",
            "thinking_end",
            "text_start",
            "text_delta",
            "text_end",
            "done"
        ]
    );
    let StreamEvent::Done { message, .. } = events.last().expect("done") else {
        panic!("expected done event");
    };
    let rpi_ai::types::AssistantContent::Thinking(thinking) = &message.content[0] else {
        panic!("expected thinking block at index 0");
    };
    assert_eq!(thinking.thinking, "thinking... more");
    // The signature arrived only on the first delta; it is retained.
    assert_eq!(thinking.thinking_signature.as_deref(), Some("QUJDREVGRw=="));
    assert_eq!(message.usage.reasoning, Some(12));
}

// ---------------------------------------------------------------------------
// Tool-call stream (no function-call streaming + auto-incrementing ids)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_google_tool_call_stream() {
    let (base_url, mut captured) = serve(vec![(200, GOOGLE_TOOL_CALL_SSE)]).await;
    let m = model(&base_url);
    let ctx = rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: None,
        messages: vec![user_text("hi")],
        tools: Some(vec![make_tool(json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"],
        }))]),
    });
    let events = collect(GoogleGenerativeAi.stream(&m, &ctx, Some(options()))).await;

    let request = captured.recv().await.expect("request captured");
    let body = request.body_json();
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["name"],
        json!("test_tool")
    );
    assert!(
        body["tools"][0]["functionDeclarations"][0]
            .get("parametersJsonSchema")
            .is_some()
    );
    // No toolChoice and no strict tools: no toolConfig.
    assert!(body.get("toolConfig").is_none());

    assert_eq!(
        event_kinds(&events),
        vec![
            "start",
            "text_start",
            "text_delta",
            "text_end",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "done"
        ]
    );
    // No function call streaming: a single delta carries the full arguments.
    let StreamEvent::ToolCallDelta { delta, .. } = &events[5] else {
        panic!("expected toolcall_delta");
    };
    assert_eq!(delta, "{\"command\":\"ls\"}");

    let StreamEvent::Done { reason, message } = events.last().expect("done") else {
        panic!("expected done event");
    };
    // A tool call overrides the STOP finish reason.
    assert_eq!(*reason, rpi_ai::types::DoneReason::ToolUse);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    let rpi_ai::types::AssistantContent::ToolCall(call) = &message.content[1] else {
        panic!("expected tool call at index 1");
    };
    assert_eq!(call.name, "bash");
    assert_eq!(call.arguments["command"], json!("ls"));
    // Unsigned upstream part id → generated `{name}_{ts}_{counter}` id.
    assert!(
        call.id.starts_with("bash_"),
        "generated id has the name prefix: {}",
        call.id
    );
    assert_eq!(call.thought_signature.as_deref(), Some("QUJDREVGRw=="));
}

/// 5093641a5 (#8059): a tool call riding a non-`STOP` finish reason does
/// **not** upgrade the stop reason to toolUse — only a clean stop does.
#[tokio::test]
async fn test_google_tool_call_with_length_finish_keeps_length() {
    const LENGTH_TOOL_CALL_SSE: &str = concat!(
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"bash\",\"args\":{\"command\":\"ls\"}}}]},\"finishReason\":\"MAX_TOKENS\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":5,\"totalTokenCount\":15}}\n",
        "\n",
    );
    let (base_url, _captured) = serve(vec![(200, LENGTH_TOOL_CALL_SSE)]).await;
    let m = model(&base_url);
    let ctx = rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: None,
        messages: vec![user_text("hi")],
        tools: Some(vec![make_tool(json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"],
        }))]),
    });
    let events = collect(GoogleGenerativeAi.stream(&m, &ctx, Some(options()))).await;

    let Some(StreamEvent::Done { reason, message }) = events.last() else {
        panic!("expected done event, got {events:?}");
    };
    // The mapped MAX_TOKENS (length) survives; the tool call alone no longer
    // forces toolUse.
    assert_eq!(*reason, rpi_ai::types::DoneReason::Length);
    assert_eq!(message.stop_reason, StopReason::Length);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("MAX_TOKENS"));
    assert!(
        message
            .content
            .iter()
            .any(|block| matches!(block, rpi_ai::types::AssistantContent::ToolCall(_)))
    );
}

#[tokio::test]
async fn test_google_tool_choice_any_sets_validated_mode_off() {
    let (base_url, mut captured) = serve(vec![(200, GOOGLE_TOOL_CALL_SSE)]).await;
    let m = model(&base_url);
    let ctx = rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: None,
        messages: vec![user_text("hi")],
        tools: Some(vec![make_tool(json!({"type": "object", "properties": {}}))]),
    });
    let events = collect(rpi_ai::api::google_generative_ai::stream(
        &m,
        &ctx,
        rpi_ai::api::google_generative_ai::GoogleOptions {
            stream: options(),
            tool_choice: Some(rpi_ai::api::google_generative_ai::GoogleToolChoice::Any),
            thinking: None,
        },
    ))
    .await;
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));

    let request = captured.recv().await.expect("request captured");
    let body = request.body_json();
    assert_eq!(
        body["toolConfig"],
        json!({"functionCallingConfig": {"mode": "ANY"}})
    );
}

// ---------------------------------------------------------------------------
// Error flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_google_http_error_surfaces_body() {
    let error_body =
        "{\"error\":{\"code\":400,\"message\":\"bad request\",\"status\":\"INVALID_ARGUMENT\"}}";
    let (base_url, _captured) = serve(vec![(400, error_body)]).await;
    let m = model(&base_url);
    let events =
        collect(GoogleGenerativeAi.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;
    assert_eq!(event_kinds(&events), vec!["error"]);
    let StreamEvent::Error { error, .. } = &events[0] else {
        panic!("expected error event");
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    // SDK `throwErrorIfNotOK`: JSON bodies are stringified verbatim into the
    // error message.
    assert_eq!(error.error_message.as_deref(), Some(error_body));
}

#[tokio::test]
async fn test_google_in_stream_error_chunk() {
    // A raw (non-SSE) JSON error body on a 200 response: the SDK's
    // processStreamResponse chunk probe aborts with `got status: ...`.
    let (base_url, _captured) = serve(vec![(
        200,
        "{\"error\":{\"code\":429,\"message\":\"quota\",\"status\":\"RESOURCE_EXHAUSTED\"}}",
    )])
    .await;
    let m = model(&base_url);
    let events =
        collect(GoogleGenerativeAi.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;
    assert_eq!(event_kinds(&events), vec!["start", "error"]);
    let StreamEvent::Error { error, .. } = &events[1] else {
        panic!("expected error event");
    };
    let message = error.error_message.as_deref().expect("error message");
    assert!(
        message.starts_with("got status: RESOURCE_EXHAUSTED. "),
        "{message}"
    );
}

#[tokio::test]
async fn test_google_stream_without_finish_reason_is_error() {
    let (base_url, _captured) = serve(vec![(200, GOOGLE_NO_FINISH_SSE)]).await;
    let m = model(&base_url);
    let events =
        collect(GoogleGenerativeAi.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;
    assert_eq!(
        event_kinds(&events),
        vec!["start", "text_start", "text_delta", "text_end", "error"]
    );
    let StreamEvent::Error { error, .. } = events.last().expect("error") else {
        panic!("expected error event");
    };
    assert_eq!(
        error.error_message.as_deref(),
        Some("Google stream ended without a finish reason")
    );
}

// ---------------------------------------------------------------------------
// Raw stop reasons (google-raw-stop-reason.test.ts: 23cb385b6, text unified
// by 5a2539a7b @ 4181f66)
// ---------------------------------------------------------------------------

/// google-raw-stop-reason.test.ts: "preserves raw Gemini finish reasons for
/// Google Generative AI errors".
#[tokio::test]
async fn preserves_raw_gemini_finish_reasons_for_google_generative_ai_errors() {
    const MALFORMED_SSE: &str = concat!(
        "data: {\"responseId\":\"google-response-id\",\"candidates\":[{\"finishReason\":\"MALFORMED_FUNCTION_CALL\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":0,\"totalTokenCount\":1}}\n",
        "\n",
    );
    let (base_url, _captured) = serve(vec![(200, MALFORMED_SSE)]).await;
    let m = model(&base_url);
    let events =
        collect(GoogleGenerativeAi.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;
    let StreamEvent::Error { error, .. } = events.last().expect("error") else {
        panic!("expected error event, got {events:?}");
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(
        error.raw_stop_reason.as_deref(),
        Some("MALFORMED_FUNCTION_CALL")
    );
    assert_eq!(
        error.error_message.as_deref(),
        Some("Provider stopped with: MALFORMED_FUNCTION_CALL")
    );
}

/// 8b5899dce: `streamSimple` throws before returning a stream when request
/// auth is missing (types.ts:325-330) — the Rust equivalent is the
/// synchronous `Err` return, not an in-stream error event.
#[test]
fn test_google_stream_simple_without_api_key_errors() {
    let m = model("http://127.0.0.1:1");
    assert_eq!(
        stream_simple(
            &m,
            &context(vec![user_text("hi")]),
            Some(SimpleStreamOptions::default())
        )
        .err(),
        Some("No API key for provider: google".to_owned())
    );
}

// ---------------------------------------------------------------------------
// Anchor: thinking split and budget tier table (wire assertions covering the intent of
// google-thinking-disable.test.ts — upstream is an env-gated live E2E)
// ---------------------------------------------------------------------------

async fn assert_thinking_config(
    model_id: &str,
    reasoning: Option<ThinkingLevel>,
    budgets: Option<ThinkingBudgets>,
    expected: Value,
) {
    let (base_url, mut captured) = serve(vec![(200, GOOGLE_TEXT_SSE)]).await;
    let m = model_with_id(model_id, &base_url, json!({}));
    let events = collect(
        stream_simple(
            &m,
            &context(vec![user_text("hi")]),
            Some(simple_options(reasoning, budgets)),
        )
        .expect("stream_simple"),
    )
    .await;
    assert!(
        matches!(events.last(), Some(StreamEvent::Done { .. })),
        "{model_id}: stream completes"
    );
    let request = captured.recv().await.expect("request captured");
    let body = request.body_json();
    assert_eq!(
        body["generationConfig"]["thinkingConfig"], expected,
        "{model_id}: thinkingConfig"
    );
}

#[tokio::test]
async fn test_budget_table_gemini_2_5_pro() {
    for (level, budget) in [
        (ThinkingLevel::Minimal, 128),
        (ThinkingLevel::Low, 2048),
        (ThinkingLevel::Medium, 8192),
        (ThinkingLevel::High, 32768),
    ] {
        assert_thinking_config(
            "gemini-2.5-pro",
            Some(level),
            None,
            json!({"includeThoughts": true, "thinkingBudget": budget}),
        )
        .await;
    }
}

#[tokio::test]
async fn test_budget_table_gemini_2_5_flash_lite() {
    for (level, budget) in [
        (ThinkingLevel::Minimal, 512),
        (ThinkingLevel::Low, 2048),
        (ThinkingLevel::Medium, 8192),
        (ThinkingLevel::High, 24576),
    ] {
        assert_thinking_config(
            "gemini-2.5-flash-lite",
            Some(level),
            None,
            json!({"includeThoughts": true, "thinkingBudget": budget}),
        )
        .await;
    }
}

#[tokio::test]
async fn test_budget_table_gemini_2_5_flash() {
    for (level, budget) in [
        (ThinkingLevel::Minimal, 128),
        (ThinkingLevel::Low, 2048),
        (ThinkingLevel::Medium, 8192),
        (ThinkingLevel::High, 24576),
    ] {
        assert_thinking_config(
            "gemini-2.5-flash",
            Some(level),
            None,
            json!({"includeThoughts": true, "thinkingBudget": budget}),
        )
        .await;
    }
}

#[tokio::test]
async fn test_budget_dynamic_minus_one_for_other_models() {
    assert_thinking_config(
        "gemini-2.0-flash",
        Some(ThinkingLevel::High),
        None,
        json!({"includeThoughts": true, "thinkingBudget": -1}),
    )
    .await;
}

#[tokio::test]
async fn test_custom_thinking_budgets_win() {
    let budgets = ThinkingBudgets {
        minimal: None,
        low: None,
        medium: Some(555),
        high: None,
    };
    assert_thinking_config(
        "gemini-2.5-pro",
        Some(ThinkingLevel::Medium),
        Some(budgets),
        json!({"includeThoughts": true, "thinkingBudget": 555}),
    )
    .await;
}

/// #9455 (16235fd93): native levels are preserved — the per-family
/// collapse tables (gemini-3-pro medium→HIGH, gemma-4 low→MINIMAL) are gone;
/// each model's `thinkingLevelMap` carries family capability. G2: old
/// expectations `{medium → HIGH}` / `{low → MINIMAL}` were the pre-#9455
/// collapse behavior.
#[tokio::test]
async fn test_thinking_level_split_gemini_3_and_gemma_4() {
    // Gemini 3 Pro: native levels (upstream "preserves native medium effort
    // for Gemini 3.1 Pro").
    assert_thinking_config(
        "gemini-3-pro-preview",
        Some(ThinkingLevel::Low),
        None,
        json!({"includeThoughts": true, "thinkingLevel": "LOW"}),
    )
    .await;
    assert_thinking_config(
        "gemini-3.1-pro-preview",
        Some(ThinkingLevel::Medium),
        None,
        json!({"includeThoughts": true, "thinkingLevel": "MEDIUM"}),
    )
    .await;
    // Gemini 3 Flash: full level range.
    assert_thinking_config(
        "gemini-3-flash-preview",
        Some(ThinkingLevel::Minimal),
        None,
        json!({"includeThoughts": true, "thinkingLevel": "MINIMAL"}),
    )
    .await;
    assert_thinking_config(
        "gemini-flash-latest",
        Some(ThinkingLevel::Medium),
        None,
        json!({"includeThoughts": true, "thinkingLevel": "MEDIUM"}),
    )
    .await;
    // Gemma 4: native levels too (low → LOW, medium → MEDIUM).
    assert_thinking_config(
        "gemma-4-27b-it",
        Some(ThinkingLevel::Low),
        None,
        json!({"includeThoughts": true, "thinkingLevel": "LOW"}),
    )
    .await;
    assert_thinking_config(
        "gemma-4-27b-it",
        Some(ThinkingLevel::High),
        None,
        json!({"includeThoughts": true, "thinkingLevel": "HIGH"}),
    )
    .await;
}

/// #9455 (16235fd93): disabling uses each model's own supported levels —
/// mapless models (off available) fully disable via `thinkingBudget: 0`;
/// level-controlled models whose map excludes off fall back to their lowest
/// supported discrete level. G2: old expectations pinned family-hardcoded
/// LOW/MINIMAL for every level-controlled model regardless of map.
#[tokio::test]
async fn test_thinking_disable_configs() {
    // Gemini 2.x disables via thinkingBudget = 0.
    assert_thinking_config("gemini-2.5-flash", None, None, json!({"thinkingBudget": 0})).await;
    // Mapless Gemini 3 / Gemma 4 (off available): budget-based disable too.
    assert_thinking_config(
        "gemini-3-pro-preview",
        None,
        None,
        json!({"thinkingBudget": 0}),
    )
    .await;
    assert_thinking_config("gemma-4-27b-it", None, None, json!({"thinkingBudget": 0})).await;
    // Upstream #9455 regression "uses the lowest supported level when
    // reasoning is omitted": gemini-3.8-flash without off falls back to LOW
    // (no includeThoughts — hidden thinking stays invisible).
    let (base_url, mut captured) = serve(vec![(200, GOOGLE_TEXT_SSE)]).await;
    let m = model_with_id(
        "gemini-3.8-flash",
        &base_url,
        json!({"thinkingLevelMap": {
            "off": null, "minimal": null, "low": "low", "medium": "medium",
            "high": "high", "xhigh": null, "max": null
        }}),
    );
    let events = collect(
        stream_simple(
            &m,
            &context(vec![user_text("hi")]),
            Some(simple_options(None, None)),
        )
        .expect("stream_simple"),
    )
    .await;
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    let request = captured.recv().await.expect("request captured");
    let body = request.body_json();
    assert_eq!(
        body["generationConfig"]["thinkingConfig"],
        json!({"thinkingLevel": "LOW"})
    );
}

// ---------------------------------------------------------------------------
// Ported intent of upstream test: google-thinking-signature.test.ts
// ---------------------------------------------------------------------------

#[test]
fn test_is_thinking_part_thought_true_only() {
    assert!(is_thinking_part(Some(true)));
    // thoughtSignature alone never marks thinking content.
    assert!(!is_thinking_part(None));
    assert!(!is_thinking_part(Some(false)));
}

#[test]
fn test_retain_thought_signature() {
    let first = retain_thought_signature(None, Some("sig-1"));
    assert_eq!(first.as_deref(), Some("sig-1"));
    let second = retain_thought_signature(first.as_deref(), None);
    assert_eq!(second.as_deref(), Some("sig-1"));
    let third = retain_thought_signature(second.as_deref(), Some(""));
    assert_eq!(third.as_deref(), Some("sig-1"));
    // A new non-empty signature replaces the old one.
    let updated = retain_thought_signature(third.as_deref(), Some("sig-2"));
    assert_eq!(updated.as_deref(), Some("sig-2"));
}

// ---------------------------------------------------------------------------
// Ported intent of upstream test: google-shared-convert-tools.test.ts
// ---------------------------------------------------------------------------

#[test]
fn test_convert_tools_strips_meta_keys_with_use_parameters() {
    let tools = vec![make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "$id": "urn:bash-tool",
        "$comment": "A bash tool for demonstration",
        "$defs": {"commandDef": {"type": "string"}},
        "definitions": {"legacyDef": {"type": "number"}},
        "type": "object",
        "properties": {"command": {"type": "string"}},
        "required": ["command"],
    }))];
    let result = convert_tools(&tools, true, true)
        .expect("converted")
        .expect("tools");
    let decl = &result[0]["functionDeclarations"][0];
    assert_eq!(
        decl["parameters"],
        json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"],
        })
    );
}

#[test]
fn test_convert_tools_strips_nested_meta_keys_recursively() {
    let tools = vec![make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "deep": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "$id": "urn:nested",
                "type": "string",
            },
        },
    }))];
    let result = convert_tools(&tools, true, true)
        .expect("converted")
        .expect("tools");
    assert_eq!(
        result[0]["functionDeclarations"][0]["parameters"],
        json!({
            "type": "object",
            "properties": {"deep": {"type": "string"}},
        })
    );
}

#[test]
fn test_convert_tools_preserves_ref_while_stripping_meta_keys() {
    let tools = vec![make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "refProp": {"$ref": "#/$defs/someDef", "type": "string"},
        },
    }))];
    let result = convert_tools(&tools, true, true)
        .expect("converted")
        .expect("tools");
    assert_eq!(
        result[0]["functionDeclarations"][0]["parameters"],
        json!({
            "type": "object",
            "properties": {
                "refProp": {"$ref": "#/$defs/someDef", "type": "string"},
            },
        })
    );
}

#[test]
fn test_convert_tools_does_not_mutate_original_parameters() {
    let original = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {"command": {"type": "string"}},
        "required": ["command"],
    });
    let tools = vec![make_tool(original.clone())];
    let _ = convert_tools(&tools, true, true);
    assert_eq!(tools[0].parameters, original);
}

#[test]
fn test_convert_tools_preserves_schema_in_parameters_json_schema() {
    let tools = vec![make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {"command": {"type": "string"}},
        "required": ["command"],
    }))];
    let result = convert_tools(&tools, false, true)
        .expect("converted")
        .expect("tools");
    assert_eq!(
        result[0]["functionDeclarations"][0]["parametersJsonSchema"],
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"],
        })
    );
}

#[test]
fn test_convert_tools_handles_tools_without_schema_meta() {
    let tools = vec![make_tool(json!({
        "type": "object",
        "properties": {"path": {"type": "string"}},
        "required": ["path"],
    }))];
    let result = convert_tools(&tools, true, true)
        .expect("converted")
        .expect("tools");
    assert_eq!(
        result[0]["functionDeclarations"][0]["parameters"],
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
        })
    );
}

#[test]
fn test_convert_tools_empty_list_is_none() {
    assert!(convert_tools(&[], false, true).expect("ok").is_none());
    assert!(convert_tools(&[], true, true).expect("ok").is_none());
}

#[test]
fn test_strict_tools_use_validated_mode_on_gemini_3() {
    let mut tool = make_tool(json!({"type": "object", "properties": {}}));
    tool.constrained_sampling = serde_json::from_value(json!({
        "type": "json_schema", "strict": "require"
    }))
    .expect("constrainedSampling");

    assert!(supports_google_strict_tool_sampling(
        "gemini-3.1-pro-preview"
    ));
    assert!(!supports_google_strict_tool_sampling("gemini-2.5-pro"));
    assert_eq!(
        resolve_google_function_calling_mode(&[tool.clone()], None, true).expect("mode"),
        Some("VALIDATED")
    );
    let error =
        resolve_google_function_calling_mode(&[tool], None, false).expect_err("strict unsupported");
    assert!(
        error.contains("requires JSON-schema constrained sampling"),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// Ported intent of upstream test: google-shared-gemini3-unsigned-tool-call.test.ts
// ---------------------------------------------------------------------------

fn gemini3_model(id: &str) -> Model {
    model_with_id(id, "https://example.com", json!({}))
}

fn tool_call_context(
    model: &Model,
    thought_signature: Option<&str>,
) -> rpi_ai::types::TranscriptContext {
    let mut call1 = json!({
        "type": "toolCall", "id": "call_1", "name": "bash",
        "arguments": {"command": "echo hi"},
    });
    if let Some(signature) = thought_signature {
        call1["thoughtSignature"] = json!(signature);
    }
    let assistant: Message = serde_json::from_value(json!({
        "role": "assistant",
        "content": [
            call1,
            {"type": "toolCall", "id": "call_2", "name": "bash", "arguments": {"command": "ls -la"}},
        ],
        "api": model.api, "provider": model.provider, "model": model.id,
        "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                  "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}},
        "stopReason": "toolUse", "timestamp": 0
    }))
    .expect("assistant");
    // cbaca6038: the upstream context carries the matching tool results so
    // function-response ids are covered too.
    let tool_result = |id: &str, text: &str| -> Message {
        serde_json::from_value(json!({
            "role": "toolResult", "toolCallId": id, "toolName": "bash",
            "content": [{"type": "text", "text": text}], "isError": false, "timestamp": 0
        }))
        .expect("toolResult")
    };
    context(vec![
        user_text("Hi"),
        assistant,
        tool_result("call_1", "hi"),
        tool_result("call_2", "files"),
    ])
}

fn function_call_parts(contents: &[Value]) -> Vec<&Value> {
    let model_turn = contents
        .iter()
        .find(|content| content["role"] == json!("model"))
        .expect("model turn");
    model_turn["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .filter(|part| part.get("functionCall").is_some())
        .collect()
}

/// All `functionResponse` parts across turns.
fn function_response_parts(contents: &[Value]) -> Vec<&Value> {
    contents
        .iter()
        .flat_map(|content| {
            content["parts"]
                .as_array()
                .map(|parts| parts.as_slice())
                .unwrap_or(&[])
        })
        .filter(|part| part.get("functionResponse").is_some())
        .collect()
}

/// cbaca6038: "preserves tool call IDs for $id via $api history" — Gemini
/// 3.x+ requires explicit tool call IDs in function calls AND responses.
#[test]
fn test_convert_messages_preserves_tool_call_ids_for_gemini_3() {
    let vertex_model = model_with_id(
        "gemini-3-pro-preview",
        "https://example.com",
        json!({"api": "google-vertex", "provider": "google-vertex"}),
    );
    for model in [
        gemini3_model("gemini-3-pro-preview"),
        gemini3_model("gemini-3.6-flash"),
        vertex_model,
    ] {
        let contents = convert_messages(&model, &tool_call_context(&model, None));
        let function_call_ids: Vec<&str> = function_call_parts(&contents)
            .iter()
            .filter_map(|part| part["functionCall"]["id"].as_str())
            .collect();
        let function_response_ids: Vec<&str> = function_response_parts(&contents)
            .iter()
            .filter_map(|part| part["functionResponse"]["id"].as_str())
            .collect();
        assert_eq!(
            function_call_ids,
            ["call_1", "call_2"],
            "function call ids for {} via {}",
            model.id,
            model.api.as_str()
        );
        assert_eq!(
            function_response_ids,
            ["call_1", "call_2"],
            "function response ids for {} via {}",
            model.id,
            model.api.as_str()
        );
    }
}

#[test]
fn test_convert_messages_unsigned_tool_calls_have_no_signature() {
    // Different assistant model id → not same provider/model → no signature.
    let model = gemini3_model("gemini-3-pro-preview");
    let mut ctx_model = model.clone();
    ctx_model.id = "other-model".to_owned();
    let contents = convert_messages(&model, &tool_call_context(&ctx_model, None));

    let parts = function_call_parts(&contents);
    assert_eq!(parts.len(), 2);
    assert!(parts[0].get("thoughtSignature").is_none());
    assert!(parts[1].get("thoughtSignature").is_none());
    let serialized = serde_json::to_string(&contents).expect("json");
    assert!(!serialized.contains("skip_thought_signature_validator"));
}

#[test]
fn test_convert_messages_preserves_valid_signature_same_provider_model() {
    let model = gemini3_model("gemini-3-pro-preview");
    let valid_sig = "AAAAAAAAAAAAAAAAAAAAAA==";
    let contents = convert_messages(&model, &tool_call_context(&model, Some(valid_sig)));

    let parts = function_call_parts(&contents);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["thoughtSignature"], json!(valid_sig));
    assert!(parts[1].get("thoughtSignature").is_none());
}

#[test]
fn test_convert_messages_drops_signature_for_other_model() {
    // cbaca6038: upstream "does not add a thoughtSignature for non-Gemini-3
    // models" — Gemini < 3 also omits tool call ids on both sides.
    let model = gemini3_model("gemini-2.5-flash");
    let mut ctx_model = model.clone();
    ctx_model.id = "other-model".to_owned();
    let contents = convert_messages(
        &model,
        &tool_call_context(&ctx_model, Some("AAAAAAAAAAAAAAAAAAAAAA==")),
    );
    let parts = function_call_parts(&contents);
    assert_eq!(parts.len(), 2);
    assert!(
        parts
            .iter()
            .all(|part| part.get("thoughtSignature").is_none())
    );
    assert!(
        parts
            .iter()
            .all(|part| part["functionCall"].get("id").is_none())
    );
    let responses = function_response_parts(&contents);
    assert_eq!(responses.len(), 2);
    assert!(
        responses
            .iter()
            .all(|part| part["functionResponse"].get("id").is_none())
    );
}

// ---------------------------------------------------------------------------
// Ported intent of upstream test: google-shared-signed-empty-blocks.test.ts
// (6138f5a07 @ 4181f66, #7362)
//
// Gemini can attach `thoughtSignature` to a response part whose visible text
// is empty (e.g. a thought burst preceding a function call) and requires the
// signature echoed back on the next request. Dropping such blocks while
// rebuilding history silently breaks the reasoning chain: the model then
// intermittently ends a mid-task turn with a thought-only STOP and no tool
// call. These tests pin the rule: an empty text/thinking block is skipped
// only when it is UNSIGNED.
// ---------------------------------------------------------------------------

const VALID_SIG: &str = "AAAAAAAAAAAAAAAAAAAAAA==";

fn signed_empty_context(model: &Model, content: Value) -> rpi_ai::types::TranscriptContext {
    let assistant: Message = serde_json::from_value(json!({
        "role": "assistant",
        "content": content,
        "api": model.api, "provider": model.provider, "model": model.id,
        "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                  "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}},
        "stopReason": "toolUse", "timestamp": 0
    }))
    .expect("assistant");
    context(vec![user_text("Hi"), assistant])
}

fn model_turn_parts(contents: &[Value]) -> &[Value] {
    contents
        .iter()
        .find(|content| content["role"] == json!("model"))
        .and_then(|content| content["parts"].as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn signed_parts(contents: &[Value]) -> Vec<&Value> {
    model_turn_parts(contents)
        .iter()
        .filter(|part| part["thoughtSignature"] == json!(VALID_SIG))
        .collect()
}

/// Upstream: "keeps a signed empty thinking block so its signature is echoed
/// back".
#[test]
fn test_convert_messages_keeps_signed_empty_thinking_block() {
    let model = gemini3_model("gemini-3-pro-preview");
    let contents = convert_messages(
        &model,
        &signed_empty_context(
            &model,
            json!([
                {"type": "thinking", "thinking": "", "thinkingSignature": VALID_SIG},
                {"type": "toolCall", "id": "call_1", "name": "bash", "arguments": {"command": "ls"}},
            ]),
        ),
    );
    let signed = signed_parts(&contents);
    assert_eq!(signed.len(), 1);
    assert_eq!(signed[0]["thought"], json!(true));
}

/// Upstream: "keeps a signed empty text block the same way".
#[test]
fn test_convert_messages_keeps_signed_empty_text_block() {
    let model = gemini3_model("gemini-3-pro-preview");
    let contents = convert_messages(
        &model,
        &signed_empty_context(
            &model,
            json!([
                {"type": "text", "text": "", "textSignature": VALID_SIG},
                {"type": "toolCall", "id": "call_1", "name": "bash", "arguments": {"command": "ls"}},
            ]),
        ),
    );
    assert_eq!(signed_parts(&contents).len(), 1);
}

/// Upstream: "still drops unsigned empty blocks".
#[test]
fn test_convert_messages_drops_unsigned_empty_blocks() {
    let model = gemini3_model("gemini-3-pro-preview");
    let contents = convert_messages(
        &model,
        &signed_empty_context(
            &model,
            json!([
                {"type": "thinking", "thinking": ""},
                {"type": "text", "text": "   "},
                {"type": "toolCall", "id": "call_1", "name": "bash", "arguments": {"command": "ls"}},
            ]),
        ),
    );
    let parts = model_turn_parts(&contents);
    assert_eq!(parts.len(), 1);
    assert!(parts[0].get("functionCall").is_some());
}

/// Upstream: "still drops signed empty blocks from a different
/// provider/model (signature unusable)".
#[test]
fn test_convert_messages_drops_signed_empty_blocks_for_other_model() {
    let model = gemini3_model("gemini-3-pro-preview");
    let mut ctx_model = model.clone();
    ctx_model.id = "other-model".to_owned();
    let contents = convert_messages(
        &model,
        &signed_empty_context(
            &ctx_model,
            json!([
                {"type": "thinking", "thinking": "", "thinkingSignature": VALID_SIG},
                {"type": "text", "text": "", "textSignature": VALID_SIG},
                {"type": "toolCall", "id": "call_1", "name": "bash", "arguments": {"command": "ls"}},
            ]),
        ),
    );
    let parts = model_turn_parts(&contents);
    assert_eq!(parts.len(), 1);
    assert!(parts[0].get("functionCall").is_some());
    let model_turn = contents
        .iter()
        .find(|content| content["role"] == json!("model"))
        .expect("model turn");
    assert!(
        !serde_json::to_string(model_turn)
            .expect("json")
            .contains(VALID_SIG)
    );
}

// ---------------------------------------------------------------------------
// Ported intent of upstream test: google-shared-image-tool-result-routing.test.ts
// ---------------------------------------------------------------------------

fn image_routing_context(model: &Model) -> rpi_ai::types::TranscriptContext {
    let assistant: Message = serde_json::from_value(json!({
        "role": "assistant",
        "content": [
            {"type": "toolCall", "id": "call_a", "name": "read", "arguments": {"path": "a.txt"}},
            {"type": "toolCall", "id": "call_img", "name": "read", "arguments": {"path": "image.png"}},
            {"type": "toolCall", "id": "call_b", "name": "read", "arguments": {"path": "b.txt"}},
        ],
        "api": model.api, "provider": model.provider, "model": model.id,
        "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                  "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}},
        "stopReason": "toolUse", "timestamp": 0
    }))
    .expect("assistant");
    let tool_result = |id: &str, content: Value| -> Message {
        serde_json::from_value(json!({
            "role": "toolResult", "toolCallId": id, "toolName": "read",
            "content": content, "isError": false, "timestamp": 0
        }))
        .expect("toolResult")
    };
    context(vec![
        user_text("read the files"),
        assistant,
        tool_result("call_a", json!([{"type": "text", "text": "alpha text"}])),
        tool_result(
            "call_img",
            json!([{"type": "image", "data": "abc", "mimeType": "image/png"}]),
        ),
        tool_result("call_b", json!([{"type": "text", "text": "beta text"}])),
    ])
}

#[test]
fn test_image_tool_result_separate_turn_for_gemini_2_x() {
    let model = model_with_id(
        "gemini-2.5-flash",
        "https://example.com",
        json!({"input": ["text", "image"]}),
    );
    let contents = convert_messages(&model, &image_routing_context(&model));

    assert_eq!(contents.len(), 5);
    // All function responses merge into one user turn.
    let merged = contents[2]["parts"].as_array().expect("parts");
    assert!(
        merged
            .iter()
            .all(|part| part.get("functionResponse").is_some())
    );
    // Images go in a separate synthetic user turn for Gemini < 3.
    assert_eq!(contents[3]["role"], json!("user"));
    assert_eq!(contents[3]["parts"][0]["text"], json!("Tool result image:"));
    assert!(contents[3]["parts"][1].get("inlineData").is_some());
    assert!(contents[4]["parts"][0].get("functionResponse").is_some());
}

#[test]
fn test_image_tool_result_nested_for_gemini_3() {
    let model = model_with_id(
        "gemini-3-pro-preview",
        "https://example.com",
        json!({"input": ["text", "image"]}),
    );
    let contents = convert_messages(&model, &image_routing_context(&model));

    assert_eq!(contents.len(), 3);
    let tool_result_turn = contents[2]["parts"].as_array().expect("parts");
    assert_eq!(tool_result_turn.len(), 3);
    let image_response = &tool_result_turn[1]["functionResponse"];
    assert!(image_response.is_object());
    let nested = image_response["parts"].as_array().expect("nested parts");
    assert_eq!(nested.len(), 1);
    assert!(nested[0].get("inlineData").is_some());
}

// ---------------------------------------------------------------------------
// Remaining anchors: stop mapping / requiresToolCallId
// ---------------------------------------------------------------------------

#[test]
fn test_map_stop_reason() {
    assert_eq!(map_stop_reason("STOP").expect("stop"), StopReason::Stop);
    assert_eq!(
        map_stop_reason("MAX_TOKENS").expect("length"),
        StopReason::Length
    );
    for reason in ["SAFETY", "RECITATION", "MALFORMED_FUNCTION_CALL", "OTHER"] {
        assert_eq!(map_stop_reason(reason).expect("error"), StopReason::Error);
    }
    // Unknown reasons are an error (upstream's `never` check throws).
    assert_eq!(
        map_stop_reason("SOME_FUTURE_REASON"),
        Err("Unhandled stop reason: SOME_FUTURE_REASON".to_owned())
    );
    assert_eq!(map_stop_reason_string("STOP"), StopReason::Stop);
    assert_eq!(map_stop_reason_string("MAX_TOKENS"), StopReason::Length);
    assert_eq!(map_stop_reason_string("SAFETY"), StopReason::Error);
}

#[test]
fn test_requires_tool_call_id() {
    // cbaca6038: Gemini 3.x+ requires explicit tool call IDs (before the
    // change only claude-*/gpt-oss-* did).
    assert!(!requires_tool_call_id("gemini-2.5-flash"));
    assert!(requires_tool_call_id("gemini-3.6-flash"));
    assert!(requires_tool_call_id("claude-sonnet-4-5"));
    assert!(requires_tool_call_id("gpt-oss-120b"));
}

// ---------------------------------------------------------------------------
// Ported intent of upstream test: google-shared-retry.test.ts
// (b9d360a2c @ 4181f66, #7471) — the initial request runs under the shared
// provider retry policy via `retryGoogleRequest` (opt-in via `maxRetries`).
// Upstream drives the helper with a mocked SDK; rpi ports the intents to
// wire-level scripted servers (the rpi helper has no SDK ApiError to
// normalize — see google_shared.rs).
// ---------------------------------------------------------------------------

const GOOGLE_429_BODY: &str =
    "{\"error\":{\"code\":429,\"message\":\"quota\",\"status\":\"RESOURCE_EXHAUSTED\"}}";
const GOOGLE_400_BODY: &str =
    "{\"error\":{\"code\":400,\"message\":\"bad request\",\"status\":\"INVALID_ARGUMENT\"}}";

/// Upstream: "retries a headers-less SDK error with a retryable status".
#[tokio::test]
async fn test_google_retries_retryable_status_with_max_retries() {
    let (base_url, mut captured) =
        serve(vec![(429, GOOGLE_429_BODY), (200, GOOGLE_TEXT_SSE)]).await;
    let m = model(&base_url);
    let mut opts = options();
    opts.max_retries = Some(1);
    let events =
        collect(GoogleGenerativeAi.stream(&m, &context(vec![user_text("hi")]), Some(opts))).await;

    assert!(
        matches!(events.last(), Some(StreamEvent::Done { .. })),
        "expected done after retry, got {events:?}"
    );
    assert!(captured.recv().await.is_some(), "first request");
    assert!(captured.recv().await.is_some(), "retried request");
}

/// Upstream: "does not retry when maxRetries is unset".
#[tokio::test]
async fn test_google_does_not_retry_when_max_retries_unset() {
    let (base_url, mut captured) = serve(vec![(429, GOOGLE_429_BODY)]).await;
    let m = model(&base_url);
    let events =
        collect(GoogleGenerativeAi.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;

    let Some(StreamEvent::Error { error, .. }) = events.last() else {
        panic!("expected error event, got {events:?}");
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.error_message.as_deref(), Some(GOOGLE_429_BODY));
    assert!(captured.recv().await.is_some(), "first request");
    assert!(
        captured.recv().await.is_none(),
        "no retry without maxRetries"
    );
}

/// Upstream: "does not retry a non-retryable status".
#[tokio::test]
async fn test_google_does_not_retry_non_retryable_status() {
    let (base_url, mut captured) = serve(vec![(400, GOOGLE_400_BODY)]).await;
    let m = model(&base_url);
    let mut opts = options();
    opts.max_retries = Some(2);
    let events =
        collect(GoogleGenerativeAi.stream(&m, &context(vec![user_text("hi")]), Some(opts))).await;

    let Some(StreamEvent::Error { error, .. }) = events.last() else {
        panic!("expected error event, got {events:?}");
    };
    assert_eq!(error.error_message.as_deref(), Some(GOOGLE_400_BODY));
    assert!(captured.recv().await.is_some(), "first request");
    assert!(
        captured.recv().await.is_none(),
        "400 is not in the retryable set"
    );
}

/// The retry-after channel (R2.4.2): the 429 response's `retry-after` header
/// reaches the shared backoff policy through the captured response headers.
/// (`retry-after: 0` keeps the test fast; the parsing/capping itself is
/// pinned byte-for-byte in `utils::provider_retry` tests.)
#[tokio::test]
async fn test_google_retry_honors_retry_after_header() {
    let (base_url, mut captured) = serve_with_response_headers(vec![
        (429, vec![("retry-after", "0")], GOOGLE_429_BODY),
        (200, vec![], GOOGLE_TEXT_SSE),
    ])
    .await;
    let m = model(&base_url);
    let mut opts = options();
    opts.max_retries = Some(1);
    let events =
        collect(GoogleGenerativeAi.stream(&m, &context(vec![user_text("hi")]), Some(opts))).await;

    assert!(
        matches!(events.last(), Some(StreamEvent::Done { .. })),
        "expected done after server-directed retry, got {events:?}"
    );
    assert!(captured.recv().await.is_some(), "first request");
    assert!(captured.recv().await.is_some(), "retried request");
}
