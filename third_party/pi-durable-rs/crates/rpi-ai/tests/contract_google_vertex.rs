//! Contract tests for the Google Vertex AI adapter (T13 W2), mirroring the
//! upstream suite `google-vertex-api-key-resolution.test.ts` (ADC fallback on
//! marker/placeholder keys, real-key client selection, custom baseUrl
//! forwarding, `{location}` placeholder discard, apiVersion suppression) and
//! the requirements §5.2 anchors (API key / ADC resolution, project/location
//! resolution, baseUrl `{location}` placeholder discard).
//!
//! The SSE payloads use the Vertex AI wire format (the shapes the pinned
//! `@google/genai` SDK feeds through `generateContentResponseFromVertex`).
//! All HTTP is served by a loopback mock; no real network access.

use std::io::Write as _;
use std::time::Duration;

use futures::StreamExt;
use rpi_ai::api::google_adc::AdcEndpoints;
use rpi_ai::api::google_vertex::{
    GCP_VERTEX_CREDENTIALS_MARKER, GoogleVertex, GoogleVertexOptions,
    base_url_includes_api_version, is_placeholder_api_key, resolve_api_key,
    resolve_custom_base_url, resolve_location, resolve_project, resolve_request_url, stream,
    stream_simple,
};
use rpi_ai::models::ProviderStreams;
use rpi_ai::types::{
    ApiKind, Context, Message, Model, ProviderEnv, SimpleStreamOptions, StopReason, StreamEvent,
    StreamOptions, ThinkingLevel, Tool,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Scripted HTTP server (same harness as the other adapter contract tests)
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

/// Serves the scripted `(status, content_type, body)` responses, one per
/// connection, on a loopback port. Returns the base URL and a channel of
/// captured requests.
async fn serve(
    script: Vec<(u16, &'static str, &'static str)>,
) -> (String, mpsc::Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel(script.len().max(1));
    tokio::spawn(async move {
        for (status, content_type, body) in script {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let request = read_request(&mut socket).await;
            tx.send(request).await.expect("send captured request");
            let reason = match status {
                200 => "OK",
                400 => "Bad Request",
                403 => "Forbidden",
                _ => "Status",
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

/// One-shot SSE response shorthand.
fn sse(body: &'static str) -> (u16, &'static str, &'static str) {
    (200, "text/event-stream", body)
}

fn json_response(status: u16, body: &'static str) -> (u16, &'static str, &'static str) {
    (status, "application/json", body)
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
        "id": id, "name": id, "api": ApiKind::GOOGLE_VERTEX, "provider": "google-vertex",
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
    model_with_id("gemini-2.5-flash", base_url, json!({}))
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

fn api_key_options() -> StreamOptions {
    StreamOptions {
        request: rpi_ai::ProviderRequestOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_owned()),
            ..Default::default()
        },
        ..StreamOptions::default()
    }
}

fn vertex_options(stream: StreamOptions) -> GoogleVertexOptions {
    GoogleVertexOptions {
        stream,
        ..GoogleVertexOptions::default()
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

/// Writes `contents` to a unique temp file and returns its path (removed by
/// the caller).
fn write_temp_credential_file(tag: &str, contents: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "rpi-vertex-adc-test-{}-{}-{tag}.json",
        std::process::id(),
        now_nanos()
    ));
    let mut file = std::fs::File::create(&path).expect("create temp credential file");
    file.write_all(contents.as_bytes()).expect("write");
    path
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn env_with(pairs: &[(&str, &str)]) -> ProviderEnv {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// Decodes one form segment of a JWS compact assertion.
fn decode_jwt_segment(segment: &str) -> Value {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .expect("base64url segment");
    serde_json::from_slice(&bytes).expect("jwt segment json")
}

/// Minimal form-body parser (`application/x-www-form-urlencoded`).
fn parse_form(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).expect("hex");
                out.push(u8::from_str_radix(hex, 16).expect("hex byte"));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("utf8")
}

// ---------------------------------------------------------------------------
// Recorded SSE streams (Vertex AI wire format)
// ---------------------------------------------------------------------------

const VERTEX_TEXT_SSE: &str = concat!(
    "data: {\"responseId\":\"vertex-response-id\",\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hello\"}]}}]}\n",
    "\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\" world\"}]}}],\"usageMetadata\":{\"promptTokenCount\":10,\"cachedContentTokenCount\":4,\"candidatesTokenCount\":5,\"thoughtsTokenCount\":2,\"totalTokenCount\":21}}\n",
    "\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[]},\"finishReason\":\"STOP\"}]}\n",
    "\n",
);

const VERTEX_THINKING_SSE: &str = concat!(
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"thought\":true,\"text\":\"let me think\",\"thoughtSignature\":\"c2ln\"}]}}]}\n",
    "\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"answer\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":3,\"candidatesTokenCount\":4,\"thoughtsTokenCount\":7,\"totalTokenCount\":14}}\n",
    "\n",
);

const VERTEX_TOOL_CALL_SSE: &str = concat!(
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"get_weather\",\"args\":{\"city\":\"Paris\"}}}]}}]}\n",
    "\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":6,\"totalTokenCount\":11}}\n",
    "\n",
);

const TOKEN_RESPONSE: &str =
    "{\"access_token\": \"ya29.adc-token\", \"expires_in\": 3600, \"token_type\": \"Bearer\"}";

/// Throwaway test-only RSA key (generated for this test suite; not a real
/// credential).
const TEST_PRIVATE_KEY: &str = include_str!("fixtures/adc_test_key.pem");

// ---------------------------------------------------------------------------
// API key resolution (upstream google-vertex-api-key-resolution.test.ts)
// ---------------------------------------------------------------------------

#[test]
fn test_resolve_api_key_falls_back_to_adc_on_marker_and_placeholder() {
    // "falls back to ADC when options.apiKey is a placeholder marker"
    assert_eq!(resolve_api_key(Some("<authenticated>")), None);
    // "falls back to ADC when options.apiKey is the gcp-vertex-credentials marker"
    assert_eq!(resolve_api_key(Some(GCP_VERTEX_CREDENTIALS_MARKER)), None);
    // Empty / whitespace keys fall back as well (upstream `!apiKey`).
    assert_eq!(resolve_api_key(Some("   ")), None);
    assert_eq!(resolve_api_key(None), None);
}

#[test]
fn test_resolve_api_key_keeps_real_keys() {
    // "still uses the API key client for real API keys"
    assert_eq!(
        resolve_api_key(Some("AIzaSyExampleRealisticLookingApiKey123456")),
        Some("AIzaSyExampleRealisticLookingApiKey123456".to_owned())
    );
    // Keys are trimmed before use (upstream `options?.apiKey?.trim()`).
    assert_eq!(
        resolve_api_key(Some("  real-key  ")),
        Some("real-key".to_owned())
    );
}

#[test]
fn test_is_placeholder_api_key_shape() {
    assert!(is_placeholder_api_key("<authenticated>"));
    assert!(is_placeholder_api_key("<a>"));
    // `[^>]+` requires at least one inner char and no inner `>`.
    assert!(!is_placeholder_api_key("<>"));
    assert!(!is_placeholder_api_key("<a>b>"));
    assert!(!is_placeholder_api_key("plain"));
}

// ---------------------------------------------------------------------------
// project / location resolution
// ---------------------------------------------------------------------------

#[test]
fn test_resolve_project_precedence() {
    let env = env_with(&[
        ("GOOGLE_CLOUD_PROJECT", "env-project"),
        ("GCLOUD_PROJECT", "fallback-project"),
    ]);
    // options.project wins over the env chain.
    assert_eq!(
        resolve_project(Some("option-project"), Some(&env)).expect("project"),
        "option-project"
    );
    // GOOGLE_CLOUD_PROJECT wins over GCLOUD_PROJECT.
    assert_eq!(
        resolve_project(None, Some(&env)).expect("project"),
        "env-project"
    );
    // GCLOUD_PROJECT fallback.
    let env = env_with(&[("GCLOUD_PROJECT", "fallback-project")]);
    assert_eq!(
        resolve_project(None, Some(&env)).expect("project"),
        "fallback-project"
    );
    // Empty option value is falsy upstream and falls through to env.
    assert_eq!(
        resolve_project(Some(""), Some(&env)).expect("project"),
        "fallback-project"
    );
}

#[test]
fn test_resolve_project_missing_errors() {
    let env = env_with(&[]);
    let error = resolve_project(None, Some(&env)).unwrap_err();
    assert_eq!(
        error,
        "Vertex AI requires a project ID. Set GOOGLE_CLOUD_PROJECT/GCLOUD_PROJECT or pass project in options."
    );
}

#[test]
fn test_resolve_location_precedence_and_missing() {
    let env = env_with(&[("GOOGLE_CLOUD_LOCATION", "env-location")]);
    assert_eq!(
        resolve_location(Some("option-location"), Some(&env)).expect("location"),
        "option-location"
    );
    assert_eq!(
        resolve_location(None, Some(&env)).expect("location"),
        "env-location"
    );
    let env = env_with(&[]);
    let error = resolve_location(None, Some(&env)).unwrap_err();
    assert_eq!(
        error,
        "Vertex AI requires a location. Set GOOGLE_CLOUD_LOCATION or pass location in options."
    );
}

// ---------------------------------------------------------------------------
// Custom base URL resolution (`{location}` placeholder discard)
// ---------------------------------------------------------------------------

#[test]
fn test_resolve_custom_base_url_discards_location_placeholder() {
    // Requirements §5.2 anchor / google-vertex.ts:391-397: the placeholder is
    // NOT template-substituted; the whole baseUrl is discarded so the SDK
    // default endpoint applies.
    assert_eq!(
        resolve_custom_base_url("https://{location}-aiplatform.googleapis.com"),
        None
    );
    assert_eq!(resolve_custom_base_url(""), None);
    assert_eq!(resolve_custom_base_url("   "), None);
    // Values are trimmed before use.
    assert_eq!(
        resolve_custom_base_url("  https://proxy.example.com  "),
        Some("https://proxy.example.com".to_owned())
    );
}

#[test]
fn test_base_url_includes_api_version() {
    assert!(base_url_includes_api_version(
        "https://proxy.example.com/v1"
    ));
    assert!(base_url_includes_api_version(
        "https://proxy.example.com/v1beta1/projects/x"
    ));
    assert!(base_url_includes_api_version(
        "https://proxy.example.com/v1beta/"
    ));
    assert!(!base_url_includes_api_version("https://proxy.example.com"));
    assert!(!base_url_includes_api_version(
        "https://proxy.example.com/v1x"
    ));
    assert!(!base_url_includes_api_version(
        "https://proxy.example.com/version"
    ));
    // Unparseable URLs fall back to the raw-string scan (upstream catch arm).
    assert!(base_url_includes_api_version("not a url/v2/path"));
    assert!(!base_url_includes_api_version("not a url at all"));
}

// ---------------------------------------------------------------------------
// Endpoint / URL selection (SDK ApiClient reverse-engineering)
// ---------------------------------------------------------------------------

#[test]
fn test_request_url_api_key_mode_uses_global_host_without_project_prefix() {
    let model = model("https://{location}-aiplatform.googleapis.com");
    let url = resolve_request_url(
        &model,
        true,
        None,
        None,
        "publishers/google/models/gemini-2.5-flash",
    );
    assert_eq!(
        url,
        "https://aiplatform.googleapis.com/v1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );
}

#[test]
fn test_request_url_adc_regional_host_prepends_project_location() {
    // "does not forward generated Vertex base URL placeholders": the
    // placeholder baseUrl is discarded and the SDK default endpoint for the
    // resolved location applies.
    let model = model("https://{location}-aiplatform.googleapis.com");
    let url = resolve_request_url(
        &model,
        false,
        Some("test-project"),
        Some("us-central1"),
        "publishers/google/models/gemini-2.5-flash",
    );
    assert_eq!(
        url,
        "https://us-central1-aiplatform.googleapis.com/v1/projects/test-project/locations/us-central1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );
}

#[test]
fn test_request_url_adc_global_and_multi_regional_hosts() {
    let model = model("https://{location}-aiplatform.googleapis.com");
    let path = "publishers/google/models/gemini-2.5-flash";
    assert_eq!(
        resolve_request_url(&model, false, Some("p"), Some("global"), path),
        "https://aiplatform.googleapis.com/v1/projects/p/locations/global/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );
    // MULTI_REGIONAL_LOCATIONS = us / eu.
    assert_eq!(
        resolve_request_url(&model, false, Some("p"), Some("eu"), path),
        "https://aiplatform.eu.rep.googleapis.com/v1/projects/p/locations/eu/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );
}

#[test]
fn test_request_url_custom_base_url_collection_scope_and_api_version() {
    // Custom baseUrl: no project/location prefix (ResourceScope.COLLECTION),
    // apiVersion appended unless the URL already carries one.
    let plain = model("https://proxy.example.com");
    let path = "publishers/google/models/gemini-2.5-flash";
    assert_eq!(
        resolve_request_url(&plain, false, Some("p"), Some("us-central1"), path),
        "https://proxy.example.com/v1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );
    let versioned = model("https://proxy.example.com/v1/projects/test-project/locations/global");
    assert_eq!(
        resolve_request_url(&versioned, false, Some("p"), Some("us-central1"), path),
        "https://proxy.example.com/v1/projects/test-project/locations/global/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );
}

// ---------------------------------------------------------------------------
// End-to-end flows (mock HTTP), API key mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_normal_flow_text_events_and_wire_shape() {
    let (base_url, mut rx) = serve(vec![sse(VERTEX_TEXT_SSE)]).await;
    let model = model(&base_url);
    let events = collect(stream(
        &model,
        &context(vec![user_text("hi")]),
        vertex_options(api_key_options()),
    ))
    .await;

    assert_eq!(
        event_kinds(&events),
        [
            "start",
            "text_start",
            "text_delta",
            "text_delta",
            "text_end",
            "done"
        ]
    );
    let Some(StreamEvent::Done { reason, message }) = events.last() else {
        panic!("last event is done");
    };
    assert_eq!(*reason, rpi_ai::types::DoneReason::Stop);
    assert_eq!(message.stop_reason, StopReason::Stop);
    // 23cb385b6: the raw provider finish reason is preserved.
    assert_eq!(message.raw_stop_reason.as_deref(), Some("STOP"));
    assert_eq!(message.response_id.as_deref(), Some("vertex-response-id"));
    // usage: input = prompt - cached; output = candidates + thoughts.
    assert_eq!(message.usage.input, 6);
    assert_eq!(message.usage.output, 7);
    assert_eq!(message.usage.cache_read, 4);
    assert_eq!(message.usage.reasoning, Some(2));
    assert_eq!(message.usage.total_tokens, 21);

    let request = rx.recv().await.expect("captured request");
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.path,
        "/v1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );
    assert_eq!(
        request.header("x-goog-api-key"),
        Some("AIzaSyExampleRealisticLookingApiKey123456")
    );
    assert_eq!(request.header("authorization"), None);
    let body = request.body_json();
    assert_eq!(
        body["contents"],
        json!([{"role": "user", "parts": [{"text": "hi"}]}])
    );
    // generationConfig omitted when empty; maxTokens set by stream_simple only.
    assert!(body.get("generationConfig").is_none());
}

/// 8b5899dce deliberately did not touch google-vertex: missing credentials
/// stay an in-stream error (the access token resolves inside the stream
/// body). Pinned so the adapter-contract change cannot leak into vertex.
#[tokio::test]
async fn test_vertex_stream_simple_missing_credentials_stay_in_stream() {
    // Unreachable endpoint, no api key / ADC env: the entry must still hand
    // back a stream; the failure arrives as an in-stream error event.
    let model = model("http://127.0.0.1:9");
    let stream = stream_simple(
        &model,
        &context(vec![user_text("hi")]),
        Some(SimpleStreamOptions::default()),
    )
    .expect("entry returns a stream — vertex has no synchronous auth check");
    let events = collect(stream).await;
    assert!(
        matches!(events.last(), Some(StreamEvent::Error { .. })),
        "credential/connection failure must be encoded in-stream: {events:?}"
    );
}

#[tokio::test]
async fn test_thinking_flow_via_stream_simple() {
    let (base_url, mut rx) = serve(vec![sse(VERTEX_THINKING_SSE)]).await;
    let model = model(&base_url);
    let options = SimpleStreamOptions {
        stream: api_key_options(),
        reasoning: Some(ThinkingLevel::Medium),
        thinking_budgets: None,
        tool_choice: None,
    };
    let events = collect(
        stream_simple(&model, &context(vec![user_text("hi")]), Some(options))
            .expect("stream_simple"),
    )
    .await;

    assert_eq!(
        event_kinds(&events),
        [
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
    let Some(StreamEvent::Done { message, .. }) = events.last() else {
        panic!("last event is done");
    };
    let rpi_ai::types::AssistantContent::Thinking(thinking) = &message.content[0] else {
        panic!("first block is thinking");
    };
    assert_eq!(thinking.thinking, "let me think");
    assert_eq!(thinking.thinking_signature.as_deref(), Some("c2ln"));

    let request = rx.recv().await.expect("captured request");
    let body = request.body_json();
    // gemini-2.5-flash is a budget model: medium → 8192 (vertex table).
    assert_eq!(
        body["generationConfig"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingBudget": 8192})
    );
    // stream_simple clamps maxTokens to the model cap / remaining context.
    assert_eq!(body["generationConfig"]["maxOutputTokens"], json!(8192));
}

#[tokio::test]
async fn test_thinking_level_models_use_thinking_level_config() {
    let (base_url, mut rx) = serve(vec![sse(VERTEX_THINKING_SSE)]).await;
    let model = model_with_id("gemini-3-flash-preview", &base_url, json!({}));
    let options = SimpleStreamOptions {
        stream: api_key_options(),
        reasoning: Some(ThinkingLevel::Low),
        thinking_budgets: None,
        tool_choice: None,
    };
    let events = collect(
        GoogleVertex
            .stream_simple(&model, &context(vec![user_text("hi")]), Some(options))
            .expect("stream_simple"),
    )
    .await;
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));

    let request = rx.recv().await.expect("captured request");
    let body = request.body_json();
    assert_eq!(
        body["generationConfig"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingLevel": "LOW"})
    );
}

/// #9455 (16235fd93) vertex counterpart of the upstream regression matrix
/// (google-thinking-level-map.test.ts `googleAdapters` it.each): omitted
/// reasoning disables thinking through each model's own supported levels.
#[tokio::test]
async fn test_thinking_disable_uses_supported_levels() {
    // gemini-3.8-flash without off: lowest supported discrete level, no
    // includeThoughts.
    let (base_url, mut rx) = serve(vec![sse(VERTEX_TEXT_SSE)]).await;
    let model = model_with_id(
        "gemini-3.8-flash",
        &base_url,
        json!({"thinkingLevelMap": {
            "off": null, "minimal": null, "low": "low", "medium": "medium",
            "high": "high", "xhigh": null, "max": null
        }}),
    );
    let options = SimpleStreamOptions {
        stream: api_key_options(),
        reasoning: None,
        thinking_budgets: None,
        tool_choice: None,
    };
    let events = collect(
        GoogleVertex
            .stream_simple(&model, &context(vec![user_text("hi")]), Some(options))
            .expect("stream_simple"),
    )
    .await;
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    let request = rx.recv().await.expect("captured request");
    assert_eq!(
        request.body_json()["generationConfig"]["thinkingConfig"],
        json!({"thinkingLevel": "LOW"})
    );

    // gemini-2.5-flash (budget family): full disable via thinkingBudget = 0.
    let (base_url, mut rx) = serve(vec![sse(VERTEX_TEXT_SSE)]).await;
    let model = model_with_id("gemini-2.5-flash", &base_url, json!({}));
    let options = SimpleStreamOptions {
        stream: api_key_options(),
        reasoning: None,
        thinking_budgets: None,
        tool_choice: None,
    };
    let events = collect(
        GoogleVertex
            .stream_simple(&model, &context(vec![user_text("hi")]), Some(options))
            .expect("stream_simple"),
    )
    .await;
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    let request = rx.recv().await.expect("captured request");
    assert_eq!(
        request.body_json()["generationConfig"]["thinkingConfig"],
        json!({"thinkingBudget": 0})
    );
}

#[tokio::test]
async fn test_tool_call_flow() {
    let (base_url, mut rx) = serve(vec![sse(VERTEX_TOOL_CALL_SSE)]).await;
    let tool: Tool = serde_json::from_value(json!({
        "name": "get_weather", "description": "Get weather",
        "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
    }))
    .expect("tool");
    let ctx = rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: None,
        messages: vec![user_text("weather?")],
        tools: Some(vec![tool]),
    });
    let events = collect(stream(
        &model(&base_url),
        &ctx,
        vertex_options(api_key_options()),
    ))
    .await;

    assert_eq!(
        event_kinds(&events),
        [
            "start",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "done"
        ]
    );
    let Some(StreamEvent::Done { reason, message }) = events.last() else {
        panic!("last event is done");
    };
    // A tool call in the content forces stopReason toolUse (over STOP).
    assert_eq!(*reason, rpi_ai::types::DoneReason::ToolUse);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    let rpi_ai::types::AssistantContent::ToolCall(call) = &message.content[0] else {
        panic!("first block is toolCall");
    };
    assert_eq!(call.name, "get_weather");
    assert_eq!(
        call.arguments,
        json!({"city": "Paris"})
            .as_object()
            .cloned()
            .unwrap_or_default()
    );
    // Generated IDs follow `{name}_{Date.now()}_{counter}`.
    assert!(
        call.id.starts_with("get_weather_"),
        "generated id has the name prefix: {}",
        call.id
    );

    let request = rx.recv().await.expect("captured request");
    let body = request.body_json();
    assert_eq!(
        body["tools"],
        json!([{"functionDeclarations": [{
            "name": "get_weather",
            "description": "Get weather",
            "parametersJsonSchema": {"type": "object", "properties": {"city": {"type": "string"}}}
        }]}])
    );
}

#[tokio::test]
async fn test_error_flow_http_400_json_body_verbatim() {
    let (base_url, mut rx) = serve(vec![json_response(
        400,
        "{\"error\":{\"code\":400,\"message\":\"Invalid JSON payload\",\"status\":\"INVALID_ARGUMENT\"}}",
    )])
    .await;
    let events = collect(stream(
        &model(&base_url),
        &context(vec![user_text("hi")]),
        vertex_options(api_key_options()),
    ))
    .await;

    assert_eq!(event_kinds(&events), ["error"]);
    let Some(StreamEvent::Error { reason, error }) = events.last() else {
        panic!("last event is error");
    };
    assert_eq!(*reason, rpi_ai::types::ErrorReason::Error);
    assert_eq!(error.stop_reason, StopReason::Error);
    // SDK `throwErrorIfNotOK`: JSON error bodies are the message verbatim.
    assert_eq!(
        error.error_message.as_deref(),
        Some(
            "{\"error\":{\"code\":400,\"message\":\"Invalid JSON payload\",\"status\":\"INVALID_ARGUMENT\"}}"
        )
    );
    let _ = rx.recv().await;
}

#[tokio::test]
async fn test_error_flow_stream_ends_without_finish_reason() {
    let (base_url, _rx) = serve(vec![sse(
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"partial\"}]}}]}\n\n",
    )])
    .await;
    let events = collect(stream(
        &model(&base_url),
        &context(vec![user_text("hi")]),
        vertex_options(api_key_options()),
    ))
    .await;
    assert_eq!(
        event_kinds(&events),
        ["start", "text_start", "text_delta", "text_end", "error"]
    );
    let Some(StreamEvent::Error { error, .. }) = events.last() else {
        panic!("last event is error");
    };
    assert_eq!(
        error.error_message.as_deref(),
        Some("Google Vertex stream ended without a finish reason")
    );
}

// ---------------------------------------------------------------------------
// Retry wiring (b9d360a2c @ 4181f66, #7471; upstream blueprint
// google-shared-retry.test.ts — the shared-helper intents are covered in
// contract_google_generative_ai.rs): the initial Vertex request runs under
// the shared provider retry policy, opt-in via `maxRetries`.
// ---------------------------------------------------------------------------

/// 429 before the first token is retried when `max_retries` is set.
#[tokio::test]
async fn test_vertex_retries_retryable_status_with_max_retries() {
    let (base_url, mut rx) = serve(vec![
        json_response(
            429,
            "{\"error\":{\"code\":429,\"message\":\"quota\",\"status\":\"RESOURCE_EXHAUSTED\"}}",
        ),
        sse(VERTEX_TEXT_SSE),
    ])
    .await;
    let mut options = api_key_options();
    options.max_retries = Some(1);
    let events = collect(stream(
        &model(&base_url),
        &context(vec![user_text("hi")]),
        vertex_options(options),
    ))
    .await;

    assert!(
        matches!(events.last(), Some(StreamEvent::Done { .. })),
        "expected done after retry, got {events:?}"
    );
    assert!(rx.recv().await.is_some(), "first request");
    assert!(rx.recv().await.is_some(), "retried request");
}

/// A non-retryable status is terminal even with `max_retries` set.
#[tokio::test]
async fn test_vertex_does_not_retry_non_retryable_status() {
    let (base_url, mut rx) = serve(vec![json_response(
        400,
        "{\"error\":{\"code\":400,\"message\":\"Invalid JSON payload\",\"status\":\"INVALID_ARGUMENT\"}}",
    )])
    .await;
    let mut options = api_key_options();
    options.max_retries = Some(2);
    let events = collect(stream(
        &model(&base_url),
        &context(vec![user_text("hi")]),
        vertex_options(options),
    ))
    .await;

    assert_eq!(event_kinds(&events), ["error"]);
    assert!(rx.recv().await.is_some(), "first request");
    assert!(rx.recv().await.is_none(), "400 is not in the retryable set");
}

// ---------------------------------------------------------------------------
// Raw stop reasons (google-raw-stop-reason.test.ts: 23cb385b6, text unified
// by 5a2539a7b @ 4181f66)
// ---------------------------------------------------------------------------

/// google-raw-stop-reason.test.ts: "preserves raw Gemini finish reasons for
/// Google Vertex errors".
#[tokio::test]
async fn preserves_raw_gemini_finish_reasons_for_google_vertex_errors() {
    let (base_url, _rx) = serve(vec![sse(
        "data: {\"responseId\":\"google-response-id\",\"candidates\":[{\"finishReason\":\"SAFETY\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":0,\"totalTokenCount\":1}}\n\n",
    )])
    .await;
    let events = collect(stream(
        &model(&base_url),
        &context(vec![user_text("hi")]),
        vertex_options(api_key_options()),
    ))
    .await;
    let Some(StreamEvent::Error { error, .. }) = events.last() else {
        panic!("last event is error, got {events:?}");
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.raw_stop_reason.as_deref(), Some("SAFETY"));
    assert_eq!(
        error.error_message.as_deref(),
        Some("Provider stopped with: SAFETY")
    );
}

// ---------------------------------------------------------------------------
// ADC flows (mock token endpoint + mock Vertex endpoint)
// ---------------------------------------------------------------------------

/// ADC options pointing the token endpoint at the mock server and disabling
/// the well-known-file lookup, with the credential file path in
/// `StreamOptions::env`.
fn adc_options(credential_path: &std::path::Path, token_url: &str) -> GoogleVertexOptions {
    GoogleVertexOptions {
        stream: StreamOptions {
            request: rpi_ai::ProviderRequestOptions {
                api_key: Some(GCP_VERTEX_CREDENTIALS_MARKER.to_owned()),
                env: Some(env_with(&[(
                    "GOOGLE_APPLICATION_CREDENTIALS",
                    credential_path.to_str().expect("utf8 path"),
                )])),
                ..Default::default()
            },
            ..StreamOptions::default()
        },
        project: Some("test-project".to_owned()),
        location: Some("us-central1".to_owned()),
        adc_endpoints: Some(AdcEndpoints {
            token_url: token_url.to_owned(),
            metadata_token_url: "http://127.0.0.1:1/unreachable".to_owned(),
            well_known_file: Some(std::path::PathBuf::from("/nonexistent/adc.json")),
        }),
        ..GoogleVertexOptions::default()
    }
}

#[tokio::test]
async fn test_adc_service_account_jwt_bearer_flow() {
    let (base_url, mut rx) = serve(vec![
        json_response(200, TOKEN_RESPONSE),
        sse(VERTEX_TEXT_SSE),
    ])
    .await;
    let credential_path = write_temp_credential_file(
        "sa",
        &serde_json::to_string(&json!({
            "type": "service_account",
            "client_email": "svc@test-project.iam.gserviceaccount.com",
            "private_key": TEST_PRIVATE_KEY,
            "token_uri": "https://oauth2.googleapis.com/token",
            "project_id": "test-project"
        }))
        .expect("json"),
    );

    // The marker apiKey must fall back to ADC (upstream: "falls back to ADC
    // when options.apiKey is the gcp-vertex-credentials marker").
    let model = model(&base_url);
    let options = adc_options(&credential_path, &format!("{base_url}/token"));
    let events = collect(stream(&model, &context(vec![user_text("hi")]), options)).await;
    let _ = std::fs::remove_file(&credential_path);

    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));

    // First request: JWT-bearer grant to the token endpoint.
    let token_request = rx.recv().await.expect("token request");
    assert_eq!(token_request.method, "POST");
    assert_eq!(token_request.path, "/token");
    let form = parse_form(&token_request.body);
    let grant_type = form
        .iter()
        .find(|(key, _)| key == "grant_type")
        .map(|(_, value)| value.as_str());
    assert_eq!(
        grant_type,
        Some("urn:ietf:params:oauth:grant-type:jwt-bearer")
    );
    let assertion = form
        .iter()
        .find(|(key, _)| key == "assertion")
        .map(|(_, value)| value.clone())
        .expect("assertion");
    let segments: Vec<&str> = assertion.split('.').collect();
    assert_eq!(segments.len(), 3);
    let header = decode_jwt_segment(segments[0]);
    assert_eq!(header["alg"], "RS256");
    let payload = decode_jwt_segment(segments[1]);
    assert_eq!(payload["iss"], "svc@test-project.iam.gserviceaccount.com");
    assert_eq!(
        payload["scope"],
        "https://www.googleapis.com/auth/cloud-platform"
    );
    assert_eq!(payload["aud"], format!("{base_url}/token"));
    assert_eq!(
        payload["exp"].as_u64(),
        payload["iat"].as_u64().map(|iat| iat + 3600)
    );

    // Second request: the Vertex call authorized with the fetched token.
    let vertex_request = rx.recv().await.expect("vertex request");
    assert_eq!(
        vertex_request.header("authorization"),
        Some("Bearer ya29.adc-token")
    );
    assert_eq!(vertex_request.header("x-goog-api-key"), None);
    // Custom baseUrl → COLLECTION scope: no project/location prefix.
    assert_eq!(
        vertex_request.path,
        "/v1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );
}

#[tokio::test]
async fn test_adc_authorized_user_refresh_grant_flow() {
    let (base_url, mut rx) = serve(vec![
        json_response(200, TOKEN_RESPONSE),
        sse(VERTEX_TEXT_SSE),
    ])
    .await;
    let credential_path = write_temp_credential_file(
        "au",
        r#"{"type": "authorized_user", "client_id": "cid", "client_secret": "csecret", "refresh_token": "rtoken"}"#,
    );

    // Placeholder apiKeys fall back to ADC as well (upstream: "falls back to
    // ADC when options.apiKey is a placeholder marker").
    let model = model(&base_url);
    let mut options = adc_options(&credential_path, &format!("{base_url}/token"));
    options.stream.api_key = Some("<authenticated>".to_owned());
    let events = collect(stream(&model, &context(vec![user_text("hi")]), options)).await;
    let _ = std::fs::remove_file(&credential_path);

    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));

    let token_request = rx.recv().await.expect("token request");
    let form = parse_form(&token_request.body);
    let get = |name: &str| {
        form.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(get("grant_type"), Some("refresh_token"));
    assert_eq!(get("client_id"), Some("cid"));
    assert_eq!(get("client_secret"), Some("csecret"));
    assert_eq!(get("refresh_token"), Some("rtoken"));

    let vertex_request = rx.recv().await.expect("vertex request");
    assert_eq!(
        vertex_request.header("authorization"),
        Some("Bearer ya29.adc-token")
    );
}

#[tokio::test]
async fn test_adc_token_endpoint_failure_surfaces_error_event() {
    let (base_url, _rx) = serve(vec![json_response(
        400,
        "{\"error\": \"invalid_grant\", \"error_description\": \"Bad Request\"}",
    )])
    .await;
    let credential_path = write_temp_credential_file(
        "sa",
        &serde_json::to_string(&json!({
            "type": "service_account",
            "client_email": "svc@test-project.iam.gserviceaccount.com",
            "private_key": TEST_PRIVATE_KEY
        }))
        .expect("json"),
    );

    let model = model(&base_url);
    let options = adc_options(&credential_path, &format!("{base_url}/token"));
    let events = collect(stream(&model, &context(vec![user_text("hi")]), options)).await;
    let _ = std::fs::remove_file(&credential_path);

    assert_eq!(event_kinds(&events), ["error"]);
    let Some(StreamEvent::Error { error, .. }) = events.last() else {
        panic!("last event is error");
    };
    // gtoken wording: `{error}: {error_description}`.
    assert_eq!(
        error.error_message.as_deref(),
        Some("invalid_grant: Bad Request")
    );
}

#[tokio::test]
async fn test_adc_missing_credential_file_error() {
    let model = model("https://proxy.example.com");
    let options = GoogleVertexOptions {
        stream: StreamOptions {
            request: rpi_ai::ProviderRequestOptions {
                api_key: Some(GCP_VERTEX_CREDENTIALS_MARKER.to_owned()),
                env: Some(env_with(&[(
                    "GOOGLE_APPLICATION_CREDENTIALS",
                    "/nonexistent/adc-credentials.json",
                )])),
                ..Default::default()
            },
            ..StreamOptions::default()
        },
        project: Some("test-project".to_owned()),
        location: Some("us-central1".to_owned()),
        ..GoogleVertexOptions::default()
    };
    let events = collect(stream(&model, &context(vec![user_text("hi")]), options)).await;

    assert_eq!(event_kinds(&events), ["error"]);
    let Some(StreamEvent::Error { error, .. }) = events.last() else {
        panic!("last event is error");
    };
    let message = error.error_message.as_deref().expect("error message");
    assert!(
        message.starts_with(
            "Unable to read the credential file specified by the GOOGLE_APPLICATION_CREDENTIALS environment variable:"
        ),
        "upstream wrapped read error: {message}"
    );
}

#[tokio::test]
async fn test_adc_missing_project_and_location_errors() {
    let model = model("https://proxy.example.com");
    let env = env_with(&[]);

    // Missing project: upstream throws before any HTTP.
    let options = GoogleVertexOptions {
        stream: StreamOptions {
            request: rpi_ai::ProviderRequestOptions {
                api_key: Some("<authenticated>".to_owned()),
                env: Some(env.clone()),
                ..Default::default()
            },
            ..StreamOptions::default()
        },
        location: Some("us-central1".to_owned()),
        ..GoogleVertexOptions::default()
    };
    let events = collect(stream(&model, &context(vec![user_text("hi")]), options)).await;
    let Some(StreamEvent::Error { error, .. }) = events.last() else {
        panic!("last event is error");
    };
    assert_eq!(
        error.error_message.as_deref(),
        Some(
            "Vertex AI requires a project ID. Set GOOGLE_CLOUD_PROJECT/GCLOUD_PROJECT or pass project in options."
        )
    );

    // Missing location.
    let options = GoogleVertexOptions {
        stream: StreamOptions {
            request: rpi_ai::ProviderRequestOptions {
                api_key: Some("<authenticated>".to_owned()),
                env: Some(env),
                ..Default::default()
            },
            ..StreamOptions::default()
        },
        project: Some("test-project".to_owned()),
        ..GoogleVertexOptions::default()
    };
    let events = collect(stream(&model, &context(vec![user_text("hi")]), options)).await;
    let Some(StreamEvent::Error { error, .. }) = events.last() else {
        panic!("last event is error");
    };
    assert_eq!(
        error.error_message.as_deref(),
        Some(
            "Vertex AI requires a location. Set GOOGLE_CLOUD_LOCATION or pass location in options."
        )
    );
}
