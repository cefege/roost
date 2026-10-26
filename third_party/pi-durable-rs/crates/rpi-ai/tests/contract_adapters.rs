//! Contract tests: drive each T03 adapter's `stream()` over a scripted local
//! HTTP server with recorded SSE streams, and assert both sides of the
//! contract — the request shape (method / path / key headers / body JSON) and
//! the emitted `StreamEvent` sequence (design §10.2, task T03 self-check
//! list).
//!
//! The SSE payloads are recorded in the upstream wire format (the same shapes
//! the upstream vitest suites feed through the real SDK event parsers).

use std::time::Duration;

use futures::StreamExt;
use rpi_ai::api::anthropic_messages::AnthropicMessages;
use rpi_ai::api::openai_completions::OpenAiCompletions;
use rpi_ai::api::openai_responses::OpenAiResponses;
use rpi_ai::models::ProviderStreams;
use rpi_ai::types::{
    ApiKind, Context, Message, Model, SimpleStreamOptions, SimpleToolChoice, StopReason,
    StreamEvent, StreamOptions,
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

fn model(api: &str, provider: &str, base_url: &str, extra: Value) -> Model {
    let mut value = json!({
        "id": "test-model", "name": "Test", "api": api, "provider": provider,
        "baseUrl": base_url, "reasoning": false, "input": ["text"],
        "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.5, "cacheWrite": 1.0},
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

const ANTHROPIC_SSE: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"test-model\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":10,\"output_tokens\":1,\"cache_read_input_tokens\":2,\"cache_creation_input_tokens\":3}}}\n",
    "\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
    "\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n",
    "\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
    "\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n",
    "\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n",
    "\n",
);

const COMPLETIONS_SSE: &str = concat!(
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"prompt_tokens_details\":{\"cached_tokens\":4}}}\n",
    "\n",
    "data: [DONE]\n",
    "\n",
);

const RESPONSES_SSE: &str = concat!(
    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n",
    "\n",
    "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"role\":\"assistant\",\"content\":[]}}\n",
    "\n",
    "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"Hello\"}\n",
    "\n",
    "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello\",\"annotations\":[]}]}}\n",
    "\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"total_tokens\":15,\"input_tokens_details\":{\"cached_tokens\":2},\"output_tokens_details\":{\"reasoning_tokens\":0}}}}\n",
    "\n",
);

// ---------------------------------------------------------------------------
// Contract tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_anthropic_messages_contract() {
    let (base_url, mut captured) = serve(vec![(200, ANTHROPIC_SSE)]).await;
    let m = model(
        ApiKind::ANTHROPIC_MESSAGES,
        "anthropic",
        &base_url,
        json!({}),
    );
    let events =
        collect(AnthropicMessages.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;

    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/messages");
    assert_eq!(request.header("x-api-key"), Some("test-key"));
    assert!(request.header("anthropic-version").is_some());
    let body = request.body_json();
    assert_eq!(body["model"], json!("test-model"));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["messages"][0]["role"], json!("user"));

    assert_eq!(
        event_kinds(&events),
        vec!["start", "text_start", "text_delta", "text_end", "done"]
    );
    let StreamEvent::Done { reason, message } = &events[4] else {
        panic!("expected done event");
    };
    assert_eq!(*reason, rpi_ai::types::DoneReason::Stop);
    assert_eq!(message.stop_reason, StopReason::Stop);
    // Usage anchored at message_start: cached/cache-write split out of input.
    assert_eq!(message.usage.input, 10);
    assert_eq!(message.usage.cache_read, 2);
    assert_eq!(message.usage.cache_write, 3);
    assert_eq!(message.usage.output, 5);
}

#[tokio::test]
async fn test_openai_completions_contract() {
    let (base_url, mut captured) = serve(vec![(200, COMPLETIONS_SSE)]).await;
    let m = model(ApiKind::OPENAI_COMPLETIONS, "openai", &base_url, json!({}));
    let events =
        collect(OpenAiCompletions.stream(&m, &context(vec![user_text("hi")]), Some(options())))
            .await;

    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer test-key"));
    let body = request.body_json();
    assert_eq!(body["model"], json!("test-model"));
    assert_eq!(body["stream"], json!(true));
    // Default compat for openai: usage requested in-stream.
    assert_eq!(body["stream_options"], json!({"include_usage": true}));

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
    let StreamEvent::Done { message, .. } = &events[5] else {
        panic!("expected done event");
    };
    assert_eq!(message.stop_reason, StopReason::Stop);
    // Usage from the terminal chunk; cached tokens split out of prompt tokens.
    assert_eq!(message.usage.input, 6);
    assert_eq!(message.usage.cache_read, 4);
    assert_eq!(message.usage.output, 5);
}

#[tokio::test]
async fn test_openai_responses_contract() {
    let (base_url, mut captured) = serve(vec![(200, RESPONSES_SSE)]).await;
    let m = model(ApiKind::OPENAI_RESPONSES, "openai", &base_url, json!({}));
    let events =
        collect(OpenAiResponses.stream(&m, &context(vec![user_text("hi")]), Some(options()))).await;

    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/responses");
    assert_eq!(request.header("authorization"), Some("Bearer test-key"));
    let body = request.body_json();
    assert_eq!(body["model"], json!("test-model"));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["store"], json!(false));

    assert_eq!(
        event_kinds(&events),
        vec!["start", "text_start", "text_delta", "text_end", "done"]
    );
    let StreamEvent::Done { message, .. } = &events[4] else {
        panic!("expected done event");
    };
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.response_id.as_deref(), Some("resp_1"));
    assert_eq!(message.usage.input, 8);
    assert_eq!(message.usage.cache_read, 2);
}

// ---------------------------------------------------------------------------
// streamSimple toolChoice forwarding (rc.12 review, #8607)
// ---------------------------------------------------------------------------

fn simple_options(tool_choice: SimpleToolChoice) -> SimpleStreamOptions {
    SimpleStreamOptions {
        stream: options(),
        reasoning: None,
        thinking_budgets: None,
        tool_choice: Some(tool_choice),
    }
}

/// #9102 (bbb61e34a, FR-C R1): OpenRouter anthropic-compatible models send
/// `x-session-id` (not `x-session-affinity`) on the wire by default, and
/// `cacheRetention: "none"` suppresses the header entirely (upstream
/// `fireworks-models.test.ts` regression ports).
#[tokio::test]
async fn test_anthropic_openrouter_session_affinity_headers() {
    let openrouter = |base_url: &str| {
        model(
            ApiKind::ANTHROPIC_MESSAGES,
            "openrouter",
            base_url,
            json!({"id": "anthropic/claude-opus-4.8", "baseUrl": format!("{base_url}/api")}),
        )
    };

    // Default retention: x-session-id only.
    let (base_url, mut captured) = serve(vec![(200, ANTHROPIC_SSE)]).await;
    let mut stream_options = options();
    stream_options.session_id = Some("openrouter-session-1".to_owned());
    let events = collect(AnthropicMessages.stream(
        &openrouter(&base_url),
        &context(vec![user_text("hi")]),
        Some(stream_options),
    ))
    .await;
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.header("x-session-id"), Some("openrouter-session-1"));
    assert_eq!(request.header("x-session-affinity"), None);

    // cacheRetention none: no affinity header at all.
    let (base_url, mut captured) = serve(vec![(200, ANTHROPIC_SSE)]).await;
    let mut stream_options = options();
    stream_options.session_id = Some("openrouter-session-2".to_owned());
    stream_options.cache_retention = Some(rpi_ai::types::CacheRetention::None);
    let _ = collect(AnthropicMessages.stream(
        &openrouter(&base_url),
        &context(vec![user_text("hi")]),
        Some(stream_options),
    ))
    .await;
    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.header("x-session-id"), None);
    assert_eq!(request.header("x-session-affinity"), None);
}

/// #9629 (6671c6047, FR-C R3): Baseten-style session affinity (catalog
/// `sendSessionAffinityHeaders: true`, OpenAI format) routes requests to the
/// same replica — asserted at the wire with an explicit compat overlay (the
/// vendored catalog data lands with the V15-03 regen; adapter behavior is
/// upstream-test-equivalent).
#[tokio::test]
async fn test_openai_completions_baseten_session_affinity_headers() {
    let (base_url, mut captured) = serve(vec![(200, COMPLETIONS_SSE)]).await;
    let m = model(
        ApiKind::OPENAI_COMPLETIONS,
        "baseten",
        &base_url,
        json!({
            "id": "zai-org/GLM-5.2",
            "baseUrl": format!("{base_url}/v1"),
            "compat": {"sendSessionAffinityHeaders": true}
        }),
    );
    let mut stream_options = options();
    stream_options.session_id = Some("baseten-catalog-session".to_owned());
    let _ = collect(OpenAiCompletions.stream(
        &m,
        &context(vec![user_text("hi")]),
        Some(stream_options),
    ))
    .await;
    let request = captured.recv().await.expect("request captured");
    assert_eq!(
        request.header("x-session-affinity"),
        Some("baseten-catalog-session")
    );
    assert_eq!(
        request.header("x-client-request-id"),
        Some("baseten-catalog-session")
    );
}

/// anthropic-messages.ts:858 — the simple choice forwards on every branch
/// (previously dropped in rpi; enum-mapped onto the object wire form).
#[tokio::test]
async fn test_stream_simple_tool_choice_anthropic() {
    let (base_url, mut captured) = serve(vec![(200, ANTHROPIC_SSE)]).await;
    let m = model(
        ApiKind::ANTHROPIC_MESSAGES,
        "anthropic",
        &base_url,
        json!({}),
    );
    let stream = AnthropicMessages
        .stream_simple(
            &m,
            &context(vec![user_text("hi")]),
            Some(simple_options(SimpleToolChoice::None)),
        )
        .expect("stream");
    let events = collect(stream).await;
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));

    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.body_json()["tool_choice"], json!({"type": "none"}));
}

/// openai-completions streamSimple already forwarded the choice — pinned
/// here because it had zero coverage (upstream
/// openai-completions-tool-choice.test.ts streamSimple intent).
#[tokio::test]
async fn test_stream_simple_tool_choice_completions() {
    let (base_url, mut captured) = serve(vec![(200, COMPLETIONS_SSE)]).await;
    let m = model(ApiKind::OPENAI_COMPLETIONS, "openai", &base_url, json!({}));
    let stream = OpenAiCompletions
        .stream_simple(
            &m,
            &context(vec![user_text("hi")]),
            Some(simple_options(SimpleToolChoice::None)),
        )
        .expect("stream");
    let events = collect(stream).await;
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));

    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.body_json()["tool_choice"], json!("none"));
}

/// openai-responses.ts:220 — the simple choice forwards verbatim as the raw
/// `tool_choice` JSON value (previously dropped in rpi).
#[tokio::test]
async fn test_stream_simple_tool_choice_responses() {
    let (base_url, mut captured) = serve(vec![(200, RESPONSES_SSE)]).await;
    let m = model(ApiKind::OPENAI_RESPONSES, "openai", &base_url, json!({}));
    let stream = OpenAiResponses
        .stream_simple(
            &m,
            &context(vec![user_text("hi")]),
            Some(simple_options(SimpleToolChoice::Auto)),
        )
        .expect("stream");
    let events = collect(stream).await;
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));

    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.body_json()["tool_choice"], json!("auto"));
}

#[tokio::test]
async fn test_responses_interleaved_content_index() {
    // Thinking / text / tool-call deltas interleave across output_index;
    // consumers associate via content_index, not arrival order.
    let sse: &str = concat!(
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\"}}\n",
        "\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"id\":\"fc_1\",\"name\":\"bash\",\"arguments\":\"\"}}\n",
        "\n",
        "data: {\"type\":\"response.reasoning_text.delta\",\"output_index\":0,\"delta\":\"think\"}\n",
        "\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"delta\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}\n",
        "\n",
        "data: {\"type\":\"response.reasoning_text.delta\",\"output_index\":0,\"delta\":\" more\"}\n",
        "\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"summary\":[]}}\n",
        "\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"id\":\"fc_1\",\"name\":\"bash\",\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}\n",
        "\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\"}}\n",
        "\n",
    );
    let (base_url, _captured) = serve(vec![(200, sse)]).await;
    let m = model(ApiKind::OPENAI_RESPONSES, "openai", &base_url, json!({}));
    let events =
        collect(OpenAiResponses.stream(&m, &context(vec![user_text("hi")]), Some(options()))).await;

    // Deltas carry the content_index of their own block.
    let delta_indexes: Vec<usize> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ThinkingDelta { content_index, .. }
            | StreamEvent::ToolCallDelta { content_index, .. } => Some(*content_index),
            _ => None,
        })
        .collect();
    assert_eq!(delta_indexes, vec![0, 1, 0]);

    let StreamEvent::Done { message, .. } = events.last().expect("done") else {
        panic!("expected done event");
    };
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.content.len(), 2);
    let rpi_ai::types::AssistantContent::Thinking(thinking) = &message.content[0] else {
        panic!("expected thinking block at index 0");
    };
    assert_eq!(thinking.thinking, "think more");
    let rpi_ai::types::AssistantContent::ToolCall(call) = &message.content[1] else {
        panic!("expected tool call block at index 1");
    };
    assert_eq!(call.id, "call_1|fc_1");
    assert_eq!(call.arguments["cmd"], json!("ls"));
}

/// #9298 (0c7bb7c5c, FR-H R1): OpenAI-compatible Responses errors carry the
/// actual provider name, not a constant "OpenAI" prefix.
#[tokio::test]
async fn test_responses_error_names_the_actual_provider() {
    for provider in ["github-copilot", "openrouter"] {
        let (base_url, _captured) =
            serve(vec![(400, "{\"error\":{\"message\":\"bad request\"}}")]).await;
        let m = model(ApiKind::OPENAI_RESPONSES, provider, &base_url, json!({}));
        let events =
            collect(OpenAiResponses.stream(&m, &context(vec![user_text("hi")]), Some(options())))
                .await;
        let StreamEvent::Error { error, .. } = events.last().expect("error") else {
            panic!("expected error event for {provider}");
        };
        assert_eq!(
            error.error_message.as_deref(),
            Some(
                format!(
                    "{provider} API error (400): {{\"error\":{{\"message\":\"bad request\"}}}}"
                )
                .as_str()
            ),
            "{provider}"
        );
    }
    // First-party OpenAI keeps the human-readable "OpenAI" name.
    let (base_url, _captured) =
        serve(vec![(400, "{\"error\":{\"message\":\"bad request\"}}")]).await;
    let m = model(ApiKind::OPENAI_RESPONSES, "openai", &base_url, json!({}));
    let events =
        collect(OpenAiResponses.stream(&m, &context(vec![user_text("hi")]), Some(options()))).await;
    let StreamEvent::Error { error, .. } = events.last().expect("error") else {
        panic!("expected error event");
    };
    assert_eq!(
        error.error_message.as_deref(),
        Some("OpenAI API error (400): {\"error\":{\"message\":\"bad request\"}}")
    );
}

#[tokio::test]
async fn test_stream_does_not_throw_on_http_error() {
    for (api, provider, prefix) in [
        (ApiKind::ANTHROPIC_MESSAGES, "anthropic", None),
        (ApiKind::OPENAI_COMPLETIONS, "openai", None),
        (
            ApiKind::OPENAI_RESPONSES,
            "openai",
            Some("OpenAI API error (400)"),
        ),
    ] {
        let (base_url, _captured) =
            serve(vec![(400, "{\"error\":{\"message\":\"bad request\"}}")]).await;
        let m = model(api, provider, &base_url, json!({}));
        let events = match api {
            ApiKind::ANTHROPIC_MESSAGES => {
                collect(AnthropicMessages.stream(
                    &m,
                    &context(vec![user_text("hi")]),
                    Some(options()),
                ))
                .await
            }
            ApiKind::OPENAI_COMPLETIONS => {
                collect(OpenAiCompletions.stream(
                    &m,
                    &context(vec![user_text("hi")]),
                    Some(options()),
                ))
                .await
            }
            _ => {
                collect(OpenAiResponses.stream(
                    &m,
                    &context(vec![user_text("hi")]),
                    Some(options()),
                ))
                .await
            }
        };
        assert_eq!(events.len(), 1, "{api}: one terminal error event");
        let StreamEvent::Error { error, .. } = &events[0] else {
            panic!("{api}: expected error event");
        };
        assert_eq!(error.stop_reason, StopReason::Error);
        let message = error.error_message.as_deref().expect("error message");
        if let Some(prefix) = prefix {
            assert!(message.starts_with(prefix), "{api}: {message}");
        } else {
            assert!(message.contains("400"), "{api}: {message}");
        }
    }
}

#[tokio::test]
async fn test_retry_then_success() {
    // First attempt 429 (retryable), second succeeds: two requests, one done.
    let (base_url, mut captured) = serve(vec![
        (429, "{\"error\":{\"message\":\"slow down\"}}"),
        (200, COMPLETIONS_SSE),
    ])
    .await;
    let m = model(ApiKind::OPENAI_COMPLETIONS, "openai", &base_url, json!({}));
    let mut retry_options = options();
    retry_options.max_retries = Some(1);
    let events =
        collect(OpenAiCompletions.stream(&m, &context(vec![user_text("hi")]), Some(retry_options)))
            .await;

    assert!(captured.recv().await.is_some());
    assert!(captured.recv().await.is_some());
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
}

#[tokio::test]
async fn test_completions_cross_provider_tool_call_id_normalization() {
    // A foreign (anthropic) assistant message with a long tool-call id is
    // replayed to openai-completions: ids are truncated to 40 chars, and the
    // tool result keeps the normalized pairing.
    let long_id = "toolu_01XyZabcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOP";
    let foreign_assistant: Message = serde_json::from_value(json!({
        "role": "assistant",
        "content": [{"type": "toolCall", "id": long_id, "name": "bash", "arguments": {"cmd": "ls"}}],
        "api": "anthropic-messages", "provider": "anthropic", "model": "claude-x",
        "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                  "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}},
        "stopReason": "toolUse", "timestamp": 0
    }))
    .expect("assistant");
    let tool_result: Message = serde_json::from_value(json!({
        "role": "toolResult", "toolCallId": long_id, "toolName": "bash",
        "content": [{"type": "text", "text": "ok"}], "isError": false, "timestamp": 0
    }))
    .expect("tool result");

    let (base_url, mut captured) = serve(vec![(200, COMPLETIONS_SSE)]).await;
    let m = model(ApiKind::OPENAI_COMPLETIONS, "openai", &base_url, json!({}));
    let events = collect(OpenAiCompletions.stream(
        &m,
        &context(vec![foreign_assistant, tool_result]),
        Some(options()),
    ))
    .await;
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));

    let request = captured.recv().await.expect("request captured");
    let body = request.body_json();
    let normalized: String = long_id.chars().take(40).collect();
    assert_eq!(
        body["messages"][0]["tool_calls"][0]["id"],
        json!(normalized)
    );
    assert_eq!(body["messages"][1]["role"], json!("tool"));
    assert_eq!(body["messages"][1]["tool_call_id"], json!(normalized));
}
