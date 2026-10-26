//! Contract tests for the `openai-codex-responses` adapter
//! (`crates/rpi-ai/src/api/openai_codex_responses.rs`), porting the intent of
//! the upstream suites `test/openai-codex-stream.test.ts` and
//! `test/openai-codex-cache-affinity-e2e.test.ts` (mock-ized: the e2e's
//! cache-affinity assertions run against the local mock backend instead of the
//! real ChatGPT backend; `test/codex-websocket-cached-probe.ts` is a manual
//! probe whose intent is covered by the debug-stats assertions here).
//!
//! A single loopback listener plays both protocols (the adapter derives the WS
//! and SSE URLs from the same `baseUrl`): connections are sniffed with
//! `peek` — `GET` is a WebSocket upgrade (tokio-tungstenite server), anything
//! else is a plain HTTP POST answered with scripted SSE. No real network.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rpi_ai::api::codex_ws::{
    close_openai_codex_web_socket_sessions, get_openai_codex_websocket_debug_stats,
    reset_openai_codex_websocket_debug_stats, set_codex_websocket_ttls_for_tests,
};
use rpi_ai::api::openai_codex_responses::{
    OpenAiCodexResponses, OpenAiCodexResponsesOptions, stream as codex_stream,
};
use rpi_ai::models::ProviderStreams;
use rpi_ai::types::{
    ApiKind, CacheRetention, Context, Message, Model, StopReason, StreamEvent, StreamOptions,
    Transport,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

// ---------------------------------------------------------------------------
// Combined HTTP/WebSocket mock backend
// ---------------------------------------------------------------------------

/// One captured HTTP request (the SSE path).
#[derive(Debug)]
struct HttpCapture {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpCapture {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The Codex SSE path zstd-compresses request bodies; decompress when the
    /// header says so.
    fn body_json(&self) -> Value {
        match self.header("content-encoding") {
            Some("zstd") => {
                let decompressed =
                    zstd::bulk::decompress(&self.body, 16 * 1024 * 1024).expect("zstd decompress");
                serde_json::from_slice(&decompressed).expect("decompressed body is JSON")
            }
            _ => serde_json::from_slice(&self.body).expect("request body is JSON"),
        }
    }
}

/// Events the mock backend reports to the test.
#[derive(Debug)]
enum ServerEvent {
    Http(HttpCapture),
    /// WebSocket upgrade captured; carries the handshake request headers.
    WsOpen(usize, Vec<(String, String)>),
    /// A `response.create` frame on connection `conn`, request number `req`.
    WsRequest(usize, usize, Value),
}

type HandshakeHook = Arc<dyn Fn(usize) -> bool + Send + Sync>;
type RequestHook = Arc<dyn Fn(usize, usize, &Value) -> Vec<Value> + Send + Sync>;
type HttpHook = Arc<dyn Fn(usize) -> (u16, String) + Send + Sync>;

/// `HttpHook` status sentinel for a "silent body": 200 headers whose
/// content-length promises bytes that never arrive, then the socket is held
/// open — the client's body read pends indefinitely. Used by the
/// cancellation race tests (rc.12 review, P1-5): only the abort token can
/// interrupt the in-flight body read.
const HTTP_SILENT_BODY: u16 = 0;

fn accept_all() -> HandshakeHook {
    Arc::new(|_| true)
}

fn no_events() -> RequestHook {
    Arc::new(|_: usize, _: usize, _: &Value| Vec::new())
}

fn ws_hook(f: impl Fn(usize, usize, &Value) -> Vec<Value> + Send + Sync + 'static) -> RequestHook {
    Arc::new(f)
}

fn http_fixed(status: u16, body: String) -> HttpHook {
    Arc::new(move |_| (status, body.clone()))
}

fn http_hook(f: impl Fn(usize) -> (u16, String) + Send + Sync + 'static) -> HttpHook {
    Arc::new(f)
}

struct MockBackend {
    base_url: String,
    events: mpsc::UnboundedReceiver<ServerEvent>,
    ws_connections: Arc<AtomicUsize>,
    http_requests: Arc<AtomicUsize>,
}

impl MockBackend {
    async fn next_http(&mut self) -> HttpCapture {
        match tokio::time::timeout(Duration::from_secs(5), self.events.recv()).await {
            Ok(Some(ServerEvent::Http(capture))) => capture,
            other => panic!("expected HTTP request, got {other:?}"),
        }
    }

    async fn next_ws_request(&mut self) -> (usize, usize, Value) {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), self.events.recv()).await {
                Ok(Some(ServerEvent::WsRequest(conn, req, body))) => return (conn, req, body),
                // Skip opens and HTTP captures interleaved in the channel.
                Ok(Some(_)) => continue,
                other => panic!("expected WS request, got {other:?}"),
            }
        }
    }

    async fn next_ws_open(&mut self) -> (usize, Vec<(String, String)>) {
        match tokio::time::timeout(Duration::from_secs(5), self.events.recv()).await {
            Ok(Some(ServerEvent::WsOpen(conn, headers))) => (conn, headers),
            other => panic!("expected WS open, got {other:?}"),
        }
    }
}

struct BackendConfig {
    handshake_hook: HandshakeHook,
    request_hook: RequestHook,
    http_hook: HttpHook,
}

/// Starts the combined backend. `http_hook` maps 1-based request numbers to
/// `(status, body)`; `handshake_hook` decides per WS connection whether the
/// upgrade is answered (false = hang, for connect-timeout tests);
/// `request_hook` maps (conn, req, frame) to the events to send back.
async fn serve_backend(
    handshake_hook: HandshakeHook,
    request_hook: RequestHook,
    http_hook: HttpHook,
) -> MockBackend {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::unbounded_channel();
    let ws_connections = Arc::new(AtomicUsize::new(0));
    let http_requests = Arc::new(AtomicUsize::new(0));
    let config = Arc::new(BackendConfig {
        handshake_hook,
        request_hook,
        http_hook,
    });

    {
        let ws_connections = ws_connections.clone();
        let http_requests = http_requests.clone();
        tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.expect("accept");
                let config = config.clone();
                let tx = tx.clone();
                let ws_connections = ws_connections.clone();
                let http_requests = http_requests.clone();
                tokio::spawn(async move {
                    // Sniff the protocol without consuming bytes: the WS
                    // handshake is a GET, the SSE request is a POST.
                    let mut buf = [0u8; 4];
                    loop {
                        match socket.peek(&mut buf).await {
                            Ok(0) => return,
                            Ok(n) if n >= 4 => break,
                            Ok(_) => tokio::task::yield_now().await,
                            Err(_) => return,
                        }
                    }
                    if &buf[..3] == b"GET" {
                        let conn = ws_connections.fetch_add(1, Ordering::SeqCst) + 1;
                        handle_ws(socket, conn, config, tx).await;
                    } else {
                        let req = http_requests.fetch_add(1, Ordering::SeqCst) + 1;
                        handle_http(socket, req, config, tx).await;
                    }
                });
            }
        });
    }

    MockBackend {
        base_url: format!("http://{addr}"),
        events: rx,
        ws_connections,
        http_requests,
    }
}

async fn handle_http(
    mut socket: TcpStream,
    req: usize,
    config: Arc<BackendConfig>,
    tx: mpsc::UnboundedSender<ServerEvent>,
) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let n = socket.read(&mut chunk).await.expect("read");
        if n == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
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
    body.truncate(content_length);

    tx.send(ServerEvent::Http(HttpCapture {
        method,
        path,
        headers,
        body,
    }))
    .expect("send capture");

    let (status, body) = (config.http_hook)(req);
    if status == HTTP_SILENT_BODY {
        // Promise 1 MiB of SSE bytes, deliver none, keep the connection
        // open (NO `connection: close` — that would let the client treat
        // EOF as end-of-body instead of waiting).
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 1048576\r\n\r\n",
            )
            .await
            .expect("write silent-body headers");
        let mut discard = [0u8; 512];
        while let Ok(n) = socket.read(&mut discard).await {
            if n == 0 {
                break;
            }
        }
        return;
    }
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

#[allow(clippy::result_large_err)] // tungstenite's accept callback signature
async fn handle_ws(
    socket: TcpStream,
    conn: usize,
    config: Arc<BackendConfig>,
    tx: mpsc::UnboundedSender<ServerEvent>,
) {
    if !(config.handshake_hook)(conn) {
        // Never answer the upgrade (connect-timeout scenario); wait until the
        // client gives up.
        let mut socket = socket;
        let mut buf = [0u8; 512];
        while let Ok(n) = socket.read(&mut buf).await {
            if n == 0 {
                break;
            }
        }
        return;
    }

    // Capture the handshake headers while accepting.
    let captured_headers = Arc::new(Mutex::new(Vec::new()));
    let callback_headers = captured_headers.clone();
    let mut ws = tokio_tungstenite::accept_hdr_async(
        socket,
        move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
              response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let headers = request
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        value.to_str().unwrap_or("").to_owned(),
                    )
                })
                .collect();
            *callback_headers.lock().expect("headers") = headers;
            Ok(response)
        },
    )
    .await
    .expect("ws handshake");
    let headers = std::mem::take(&mut *captured_headers.lock().expect("headers"));
    tx.send(ServerEvent::WsOpen(conn, headers))
        .expect("send open");

    let mut req = 0usize;
    while let Some(message) = ws.next().await {
        match message {
            Ok(WsMessage::Text(text)) => {
                let body: Value = serde_json::from_str(&text).expect("frame is JSON");
                req += 1;
                tx.send(ServerEvent::WsRequest(conn, req, body.clone()))
                    .expect("send frame");
                for event in (config.request_hook)(conn, req, &body) {
                    ws.send(WsMessage::Text(event.to_string().into()))
                        .await
                        .expect("send event");
                }
            }
            Ok(WsMessage::Close(_)) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Serializes tests that mutate the process-global WS TTL override (and any
/// WS test that could observe its effect).
static WS_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn mock_token() -> String {
    mock_token_for("acc_test")
}

fn mock_token_for(account_id: &str) -> String {
    use base64::Engine;
    let payload = base64::engine::general_purpose::STANDARD.encode(
        serde_json::to_string(&json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": account_id}
        }))
        .expect("json"),
    );
    format!("aaa.{payload}.bbb")
}

fn model(base_url: &str, extra: Value) -> Model {
    let mut value = json!({
        "id": "gpt-5.1-codex", "name": "GPT-5.1 Codex",
        "api": ApiKind::OPENAI_CODEX_RESPONSES, "provider": "openai-codex",
        "baseUrl": base_url, "reasoning": true, "input": ["text"],
        "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.5, "cacheWrite": 1.0},
        "contextWindow": 400000, "maxTokens": 128000
    });
    value
        .as_object_mut()
        .expect("object")
        .extend(extra.as_object().cloned().unwrap_or_default());
    serde_json::from_value(value).expect("model")
}

fn context(messages: Vec<Message>) -> rpi_ai::types::TranscriptContext {
    rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: Some("You are a helpful assistant.".to_owned()),
        messages,
        tools: None,
    })
}

fn user_text(text: &str) -> Message {
    serde_json::from_value(json!({"role": "user", "content": text, "timestamp": 0})).expect("user")
}

fn sse_payload(text: &str) -> String {
    sse_payload_with(text, "resp_1", "completed")
}

fn sse_payload_with(text: &str, response_id: &str, status: &str) -> String {
    let terminal_type = if status == "incomplete" {
        "response.incomplete"
    } else {
        "response.completed"
    };
    format!(
        "{}\n\n",
        [
            format!(
                "data: {}",
                json!({"type": "response.output_item.added", "output_index": 0,
                       "item": {"type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": []}})
            ),
            format!(
                "data: {}",
                json!({"type": "response.output_text.delta", "output_index": 0, "delta": text})
            ),
            format!(
                "data: {}",
                json!({"type": "response.output_item.done", "output_index": 0,
                       "item": {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                                "content": [{"type": "output_text", "text": text}]}})
            ),
            format!(
                "data: {}",
                json!({"type": terminal_type,
                       "response": {"id": response_id, "status": status,
                                    "usage": {"input_tokens": 5, "output_tokens": 3, "total_tokens": 8,
                                              "input_tokens_details": {"cached_tokens": 0}}}})
            ),
        ]
        .join("\n\n")
    )
}

/// WS events for a plain text response.
fn ws_text_events(response_id: &str, text: &str) -> Vec<Value> {
    vec![
        json!({"type": "response.created", "response": {"id": response_id}}),
        json!({"type": "response.output_item.added", "output_index": 0,
               "item": {"type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": []}}),
        json!({"type": "response.output_item.done", "output_index": 0,
               "item": {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                        "content": [{"type": "output_text", "text": text}]}}),
        json!({"type": "response.completed",
               "response": {"id": response_id, "status": "completed",
                            "usage": {"input_tokens": 5, "output_tokens": 3, "total_tokens": 8}}}),
    ]
}

/// `ws_text_events` with a terminal `end_turn` flag (c3e7bc60a).
fn ws_text_events_end_turn(response_id: &str, text: &str, end_turn: bool) -> Vec<Value> {
    vec![
        json!({"type": "response.created", "response": {"id": response_id}}),
        json!({"type": "response.output_item.added", "output_index": 0,
               "item": {"type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": []}}),
        json!({"type": "response.output_item.done", "output_index": 0,
               "item": {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                        "content": [{"type": "output_text", "text": text}]}}),
        json!({"type": "response.completed",
               "response": {"id": response_id, "status": "completed", "end_turn": end_turn,
                            "usage": {"input_tokens": 5, "output_tokens": 3, "total_tokens": 8}}}),
    ]
}

/// `sse_payload_with` with a terminal `end_turn` flag (c3e7bc60a).
fn sse_payload_end_turn(text: &str, end_turn: bool) -> String {
    format!(
        "{}\n\n",
        [
            format!(
                "data: {}",
                json!({"type": "response.output_item.added", "output_index": 0,
                       "item": {"type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": []}})
            ),
            format!(
                "data: {}",
                json!({"type": "response.output_text.delta", "output_index": 0, "delta": text})
            ),
            format!(
                "data: {}",
                json!({"type": "response.output_item.done", "output_index": 0,
                       "item": {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                                "content": [{"type": "output_text", "text": text}]}})
            ),
            format!(
                "data: {}",
                json!({"type": "response.completed",
                       "response": {"id": "resp_1", "status": "completed", "end_turn": end_turn,
                                    "usage": {"input_tokens": 5, "output_tokens": 3, "total_tokens": 8,
                                              "input_tokens_details": {"cached_tokens": 0}}}})
            ),
        ]
        .join("\n\n")
    )
}

fn default_hooks(sse_body: &'static str) -> (HandshakeHook, RequestHook, HttpHook) {
    (
        accept_all(),
        no_events(),
        http_fixed(200, sse_body.to_owned()),
    )
}

fn sse_options(session_id: Option<&str>) -> StreamOptions {
    StreamOptions {
        transport: Some(Transport::Sse),
        session_id: session_id.map(str::to_owned),
        request: rpi_ai::ProviderRequestOptions {
            api_key: Some(mock_token()),
            ..Default::default()
        },
        ..StreamOptions::default()
    }
}

async fn collect(
    stream: rpi_ai::utils::event_stream::AssistantMessageEventStream,
) -> Vec<StreamEvent> {
    tokio::time::timeout(Duration::from_secs(15), stream.collect())
        .await
        .expect("stream completes within 15s")
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

// ---------------------------------------------------------------------------
// SSE contract tests
// ---------------------------------------------------------------------------

/// Upstream: "streams SSE responses into AssistantMessageEventStream" +
/// "zstd-compresses SSE request bodies" + the e2e's cache-affinity header
/// alignment ("handles SSE requests with aligned cache-affinity identifiers",
/// mock-ized).
/// 64eeb82a4 (#9047): a stream whose terminal `response.completed` has no
/// trailing blank line must still terminate as `done` — the shared
/// `SseDecoder::finish` (UTF-8 flush + unterminated-line dispatch +
/// trailing-event flush, sse.rs) is the equivalent of upstream's
/// Codex-parser EOF handling (openai-codex-responses.ts:779-812); this pins
/// the scenario so the equivalence cannot regress.
#[tokio::test]
async fn test_sse_eof_without_trailing_blank_line_still_completes() {
    // Same event sequence as `sse_payload`, minus the final "\n\n".
    let payload = sse_payload("Hello").trim_end_matches('\n').to_owned();
    assert!(!payload.ends_with("\n\n"), "fixture must be unterminated");
    let (handshake, request, http) = default_hooks(payload.leak());
    let backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let events = collect(OpenAiCodexResponses.stream(
        &m,
        &context(vec![user_text("Say hello")]),
        Some(sse_options(None)),
    ))
    .await;

    assert!(
        matches!(events.last(), Some(StreamEvent::Done { .. })),
        "terminal event must complete the stream: {events:?}"
    );
    let text = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(text, "Hello");
}

/// rc.12 review (P1-5 residue): the SSE body read must race the abort token —
/// upstream hands the signal to fetch, so cancellation interrupts an
/// in-flight (silent) body immediately. Pre-fix shape: `aborted()` checked
/// only synchronously before `byte_stream.next().await`, which pends on the
/// silent body until the header deadline — here no timeout is set, so the
/// stream would never terminate without the race.
#[tokio::test]
async fn test_sse_cancellation_interrupts_silent_body() {
    let (handshake, request, http) = (
        accept_all(),
        no_events(),
        http_fixed(HTTP_SILENT_BODY, String::new()),
    );
    let backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));

    let token = tokio_util::sync::CancellationToken::new();
    let mut options = sse_options(None);
    options.signal = Some(token.clone());
    tokio::spawn({
        let token = token.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            token.cancel();
        }
    });

    let started = std::time::Instant::now();
    let events = collect(OpenAiCodexResponses.stream(
        &m,
        &context(vec![user_text("Say hello")]),
        Some(options),
    ))
    .await;
    assert!(
        matches!(events.last(), Some(StreamEvent::Error { .. })),
        "cancelled silent body must terminate with an Error event: {events:?}"
    );
    assert_eq!(
        backend.http_requests.load(Ordering::SeqCst),
        1,
        "no retries after abort"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancellation must not wait for a body/deadline"
    );
}

#[tokio::test]
async fn test_sse_contract_headers_and_zstd_body() {
    let (handshake, request, http) = default_hooks(sse_payload("Hello").leak());
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let events = collect(OpenAiCodexResponses.stream(
        &m,
        &context(vec![user_text("Say hello")]),
        Some(sse_options(Some("sess-1"))),
    ))
    .await;

    let request = backend.next_http().await;
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/codex/responses");
    assert_eq!(
        request.header("authorization"),
        Some(format!("Bearer {}", mock_token()).as_str())
    );
    assert_eq!(request.header("chatgpt-account-id"), Some("acc_test"));
    assert_eq!(
        request.header("openai-beta"),
        Some("responses=experimental")
    );
    assert_eq!(request.header("originator"), Some("rpi"));
    assert_eq!(request.header("accept"), Some("text/event-stream"));
    assert_eq!(request.header("content-encoding"), Some("zstd"));
    // Cache-affinity alignment: session-id / x-client-request-id headers and
    // prompt_cache_key all carry the session id.
    assert_eq!(request.header("session-id"), Some("sess-1"));
    assert_eq!(request.header("x-client-request-id"), Some("sess-1"));
    assert!(request.header("x-api-key").is_none());

    let body = request.body_json();
    assert_eq!(body["model"], json!("gpt-5.1-codex"));
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["instructions"], json!("You are a helpful assistant."));
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["text"], json!({"verbosity": "low"}));
    assert_eq!(body["tool_choice"], json!("auto"));
    assert_eq!(body["parallel_tool_calls"], json!(true));
    assert_eq!(body["prompt_cache_key"], json!("sess-1"));
    assert_eq!(
        body["input"][0],
        json!({"role": "user", "content": [{"type": "input_text", "text": "Say hello"}]})
    );

    assert_eq!(
        event_kinds(&events),
        vec!["start", "text_start", "text_delta", "text_end", "done"]
    );
    let StreamEvent::Done { message, .. } = &events[4] else {
        panic!("expected done event");
    };
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.response_id.as_deref(), Some("resp_1"));
    assert_eq!(message.usage.input, 5);
    assert_eq!(message.usage.output, 3);

    // transport "sse" must not touch WebSocket.
    assert_eq!(backend.ws_connections.load(Ordering::SeqCst), 0);
}

/// Thinking/encrypted reasoning: encrypted_content lands in the thinking
/// block's signature; reasoning effort maps through thinkingLevelMap.
#[tokio::test]
async fn test_sse_thinking_encrypted_reasoning() {
    let sse = format!(
        "{}\n\n",
        [
            format!(
                "data: {}",
                json!({"type": "response.output_item.added", "output_index": 0,
                       "item": {"type": "reasoning", "id": "rs_1", "summary": []}})
            ),
            format!(
                "data: {}",
                json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "thinking"})
            ),
            format!(
                "data: {}",
                json!({"type": "response.output_item.done", "output_index": 0,
                       "item": {"type": "reasoning", "id": "rs_1",
                                "summary": [{"type": "summary_text", "text": "thinking"}],
                                "encrypted_content": "enc_sig_1"}})
            ),
            format!(
                "data: {}",
                json!({"type": "response.completed",
                       "response": {"id": "resp_1", "status": "completed",
                                    "usage": {"input_tokens": 5, "output_tokens": 3, "total_tokens": 8}}})
            ),
        ]
        .join("\n\n")
    );
    let (handshake, request, http) = default_hooks(sse.leak());
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(
        &backend.base_url,
        json!({"thinkingLevelMap": {"high": "high-mapped"}}),
    );
    let events = collect(codex_stream(
        &m,
        &context(vec![user_text("think")]),
        OpenAiCodexResponsesOptions {
            stream: sse_options(None),
            reasoning_effort: Some("high".to_owned()),
            ..OpenAiCodexResponsesOptions::default()
        },
    ))
    .await;

    let body = backend.next_http().await.body_json();
    assert_eq!(
        body["reasoning"],
        json!({"effort": "high-mapped", "summary": "auto"})
    );
    // No session id: no cache-affinity headers/keys.
    assert!(body.get("prompt_cache_key").is_none());

    assert_eq!(
        event_kinds(&events),
        vec![
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "done"
        ]
    );
    let StreamEvent::Done { message, .. } = &events[4] else {
        panic!("expected done event");
    };
    let rpi_ai::types::AssistantContent::Thinking(thinking) = &message.content[0] else {
        panic!("expected thinking block");
    };
    assert_eq!(thinking.thinking, "thinking");
    let signature: Value =
        serde_json::from_str(thinking.thinking_signature.as_deref().expect("signature"))
            .expect("signature json");
    assert_eq!(signature["encrypted_content"], json!("enc_sig_1"));
}

/// Tool-call flow: function_call events produce the tool-call event sequence
/// and stopReason toolUse; tool definitions land in the request body.
#[tokio::test]
async fn test_sse_tool_call_flow() {
    let sse = format!(
        "{}\n\n",
        [
            format!(
                "data: {}",
                json!({"type": "response.output_item.added", "output_index": 0,
                       "item": {"type": "function_call", "call_id": "call_1", "id": "fc_1",
                                "name": "sample_tool", "arguments": ""}})
            ),
            format!(
                "data: {}",
                json!({"type": "response.function_call_arguments.delta", "output_index": 0,
                       "delta": "{\"payload\":\"abc\"}"})
            ),
            format!(
                "data: {}",
                json!({"type": "response.output_item.done", "output_index": 0,
                       "item": {"type": "function_call", "call_id": "call_1", "id": "fc_1",
                                "name": "sample_tool", "arguments": "{\"payload\":\"abc\"}"}})
            ),
            format!(
                "data: {}",
                json!({"type": "response.completed",
                       "response": {"id": "resp_1", "status": "completed",
                                    "usage": {"input_tokens": 5, "output_tokens": 3, "total_tokens": 8}}})
            ),
        ]
        .join("\n\n")
    );
    let (handshake, request, http) = default_hooks(sse.leak());
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let tool: rpi_ai::types::Tool = serde_json::from_value(json!({
        "name": "sample_tool", "description": "Sample tool",
        "parameters": {"type": "object", "properties": {"payload": {"type": "string"}}, "required": ["payload"]}
    }))
    .expect("tool");
    let ctx = rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: None,
        messages: vec![user_text("Use the tool")],
        tools: Some(vec![tool]),
    });
    let events = collect(codex_stream(
        &m,
        &ctx,
        OpenAiCodexResponsesOptions {
            stream: sse_options(None),
            tool_choice: Some("required".to_owned()),
            ..OpenAiCodexResponsesOptions::default()
        },
    ))
    .await;

    let body = backend.next_http().await.body_json();
    assert_eq!(body["tool_choice"], json!("required"));
    assert_eq!(body["tools"][0]["name"], json!("sample_tool"));

    assert_eq!(
        event_kinds(&events),
        vec![
            "start",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "done"
        ]
    );
    let StreamEvent::Done { message, .. } = &events[4] else {
        panic!("expected done event");
    };
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    let rpi_ai::types::AssistantContent::ToolCall(call) = &message.content[0] else {
        panic!("expected tool call block");
    };
    // Codex (openai-codex provider) keeps the {call_id}|{item_id} composite id.
    assert_eq!(call.id, "call_1|fc_1");
    assert_eq!(call.arguments["payload"], json!("abc"));
}

/// Error streams: HTTP errors and WS error events become a single error event.
#[tokio::test]
async fn test_error_streams() {
    // HTTP 400: message comes from the error body.
    let (handshake, request, http) = (
        accept_all(),
        no_events(),
        http_fixed(400, r#"{"error":{"message":"bad request"}}"#.to_owned()),
    );
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let events = collect(OpenAiCodexResponses.stream(
        &m,
        &context(vec![user_text("hi")]),
        Some(sse_options(None)),
    ))
    .await;
    assert!(backend.next_http().await.method == "POST");
    assert_eq!(events.len(), 1);
    let StreamEvent::Error { error, .. } = &events[0] else {
        panic!("expected error event");
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.error_message.as_deref(), Some("bad request"));

    // HTTP 429 usage-limit without a message: friendly message is built.
    let (handshake, request, http) = (
        accept_all(),
        no_events(),
        http_fixed(
            429,
            r#"{"error":{"code":"usage_limit_reached","plan_type":"Plus"}}"#.to_owned(),
        ),
    );
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let events = collect(OpenAiCodexResponses.stream(
        &m,
        &context(vec![user_text("hi")]),
        Some(sse_options(None)),
    ))
    .await;
    assert!(backend.next_http().await.method == "POST");
    let StreamEvent::Error { error, .. } = &events[0] else {
        panic!("expected error event");
    };
    assert_eq!(
        error.error_message.as_deref(),
        Some("You have hit your ChatGPT usage limit (plus plan).")
    );

    // WS error event (non-transport): no SSE fallback, single error event.
    let _guard = WS_TEST_LOCK.lock().await;
    let (handshake, request, http) = (
        accept_all(),
        ws_hook(|_, _, _| {
            vec![json!({"type": "error", "error": {"code": "bad_code", "message": "boom"}})]
        }),
        http_fixed(200, sse_payload("unused")),
    );
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let mut options = sse_options(None);
    options.transport = Some(Transport::Websocket);
    let events =
        collect(OpenAiCodexResponses.stream(&m, &context(vec![user_text("hi")]), Some(options)))
            .await;
    let (_conn, _req, _frame) = backend.next_ws_request().await;
    let StreamEvent::Error { error, .. } = &events[events.len() - 1] else {
        panic!(
            "expected error event, got {}",
            event_kinds(&events).join(",")
        );
    };
    assert_eq!(error.error_message.as_deref(), Some("Codex error: boom"));
    assert_eq!(backend.http_requests.load(Ordering::SeqCst), 0);
}

/// Upstream: "uses exponential backoff across repeated SSE retries" — timing
/// is not faked here; the retry-after header path pins the retry loop with
/// real (1ms) delays.
#[tokio::test]
async fn test_sse_retry_then_success() {
    let sse = sse_payload("Hello");
    let http = http_hook(move |req| {
        if req <= 2 {
            (
                429,
                r#"{"error":{"code":"rate_limit_exceeded","message":"rate limited"}}"#.to_owned(),
            )
        } else {
            (200, sse.clone())
        }
    });
    // Note: retry-after-ms is set via headers in upstream; our mock responds
    // with bodies only, so the backoff path (BASE_DELAY_MS * 2^attempt) is
    // exercised with max_retries=1 below instead.
    let (handshake, request) = (accept_all(), no_events());
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let mut options = sse_options(None);
    options.max_retries = Some(2);
    let events =
        collect(OpenAiCodexResponses.stream(&m, &context(vec![user_text("hi")]), Some(options)))
            .await;
    assert!(backend.next_http().await.method == "POST");
    assert!(backend.next_http().await.method == "POST");
    assert!(backend.next_http().await.method == "POST");
    assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    assert_eq!(backend.http_requests.load(Ordering::SeqCst), 3);
}

/// Invalid JWT: fails before any HTTP request.
#[tokio::test]
async fn test_invalid_token_error() {
    let (handshake, request, http) = default_hooks(sse_payload("Hello").leak());
    let backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let mut options = sse_options(None);
    options.api_key = Some("not-a-jwt".to_owned());
    let events =
        collect(OpenAiCodexResponses.stream(&m, &context(vec![user_text("hi")]), Some(options)))
            .await;
    assert_eq!(events.len(), 1);
    let StreamEvent::Error { error, .. } = &events[0] else {
        panic!("expected error event");
    };
    assert_eq!(
        error.error_message.as_deref(),
        Some("Failed to extract accountId from token")
    );
    assert_eq!(backend.http_requests.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
// WebSocket contract tests
// ---------------------------------------------------------------------------

fn ws_options(session_id: Option<&str>, transport: Transport) -> StreamOptions {
    StreamOptions {
        transport: Some(transport),
        session_id: session_id.map(str::to_owned),
        request: rpi_ai::ProviderRequestOptions {
            api_key: Some(mock_token()),
            ..Default::default()
        },
        ..StreamOptions::default()
    }
}

/// Basic WS flow: request frame shape, handshake headers, event sequence.
#[tokio::test]
async fn test_websocket_basic_flow() {
    let _guard = WS_TEST_LOCK.lock().await;
    let (handshake, request, http) = (
        accept_all(),
        ws_hook(|_, _, _| ws_text_events("resp_1", "Hello")),
        http_fixed(200, sse_payload("unused")),
    );
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let events = collect(OpenAiCodexResponses.stream(
        &m,
        &context(vec![user_text("Say hello")]),
        Some(ws_options(Some("sess-ws"), Transport::Websocket)),
    ))
    .await;

    let (conn, headers) = backend.next_ws_open().await;
    assert_eq!(conn, 1);
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    // Bug-compatible: the handshake carries the WebSocket OpenAI-Beta value
    // (upstream's case-mismatched delete is a no-op).
    assert_eq!(
        header("openai-beta"),
        Some("responses_websockets=2026-02-06")
    );
    assert_eq!(header("originator"), Some("rpi"));
    assert_eq!(header("chatgpt-account-id"), Some("acc_test"));
    assert_eq!(header("session-id"), Some("sess-ws"));
    assert_eq!(header("x-client-request-id"), Some("sess-ws"));

    let (conn, req, frame) = backend.next_ws_request().await;
    assert_eq!((conn, req), (1, 1));
    // The frame is `{type: "response.create", ...requestBody}`, uncompressed.
    assert_eq!(frame["type"], json!("response.create"));
    assert_eq!(frame["store"], json!(false));
    assert_eq!(frame["stream"], json!(true));
    assert_eq!(frame["prompt_cache_key"], json!("sess-ws"));
    assert!(frame.get("previous_response_id").is_none());

    assert_eq!(event_kinds(&events).last(), Some(&"done"));
    let Some(StreamEvent::Done { message, .. }) = events.last() else {
        panic!("expected done");
    };
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.response_id.as_deref(), Some("resp_1"));
    assert_eq!(backend.http_requests.load(Ordering::SeqCst), 0);

    close_openai_codex_web_socket_sessions(Some("sess-ws")).await;
    reset_openai_codex_websocket_debug_stats(Some("sess-ws"));
}

/// Upstream c3e7bc60a @ 4181f66 (#7766): a boolean `response.end_turn` on the
/// terminal event is preserved as `AssistantMessage.end_turn` (debugging aid,
/// no control-flow effect) — the `expect(result.endTurn).toBe(false)`
/// assertions of openai-codex-stream.test.ts, over both transports.
#[tokio::test]
async fn test_end_turn_preserved() {
    let _guard = WS_TEST_LOCK.lock().await;

    // SSE transport: end_turn: false is preserved (not dropped as falsy).
    let (handshake, request, http) = default_hooks(sse_payload_end_turn("Hello", false).leak());
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let message = codex_stream(
        &m,
        &context(vec![user_text("Say hello")]),
        OpenAiCodexResponsesOptions {
            stream: sse_options(Some("sess-et-sse")),
            ..OpenAiCodexResponsesOptions::default()
        },
    )
    .result()
    .await
    .expect("sse result");
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.end_turn, Some(false));
    let _ = backend.next_http().await;

    // WebSocket transport: same extraction from the WS terminal event.
    let (handshake, request, http) = (
        accept_all(),
        ws_hook(|_, _, _| ws_text_events_end_turn("resp_et", "Hello", false)),
        http_fixed(200, sse_payload("unused")),
    );
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let message = codex_stream(
        &m,
        &context(vec![user_text("Say hello")]),
        OpenAiCodexResponsesOptions {
            stream: ws_options(Some("sess-et-ws"), Transport::Websocket),
            ..OpenAiCodexResponsesOptions::default()
        },
    )
    .result()
    .await
    .expect("ws result");
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.end_turn, Some(false));
    let _ = backend.next_ws_request().await;

    // No end_turn on the terminal event: the field stays unset.
    let (handshake, request, http) = default_hooks(sse_payload("Hello").leak());
    let backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let message = codex_stream(
        &m,
        &context(vec![user_text("Say hello")]),
        OpenAiCodexResponsesOptions {
            stream: sse_options(None),
            ..OpenAiCodexResponsesOptions::default()
        },
    )
    .result()
    .await
    .expect("plain result");
    assert_eq!(message.end_turn, None);
    drop(backend);

    close_openai_codex_web_socket_sessions(Some("sess-et-ws")).await;
    reset_openai_codex_websocket_debug_stats(Some("sess-et-ws"));
}

/// Upstream: "sends only response input deltas in websocket-cached mode"
/// (simplified to text messages; the delta mechanics are identical).
#[tokio::test]
async fn test_websocket_cached_delta_and_reuse() {
    let _guard = WS_TEST_LOCK.lock().await;
    let session = "delta-1";
    reset_openai_codex_websocket_debug_stats(Some(session));

    let (handshake, request, http) = (
        accept_all(),
        ws_hook(|_, req, _| ws_text_events(&format!("resp_{req}"), &format!("text{req}"))),
        http_fixed(200, sse_payload("unused")),
    );
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));

    let first = codex_stream(
        &m,
        &context(vec![user_text("hi")]),
        OpenAiCodexResponsesOptions {
            stream: ws_options(Some(session), Transport::WebsocketCached),
            ..OpenAiCodexResponsesOptions::default()
        },
    )
    .result()
    .await
    .expect("first result");
    assert_eq!(first.stop_reason, StopReason::Stop);
    let first_id = first.response_id.clone().expect("response id");

    // Second request: the conversation continues on the same connection.
    let ctx2 = context(vec![
        user_text("hi"),
        Message::Assistant(first),
        user_text("Now finish"),
    ]);
    let second = codex_stream(
        &m,
        &ctx2,
        OpenAiCodexResponsesOptions {
            stream: ws_options(Some(session), Transport::WebsocketCached),
            ..OpenAiCodexResponsesOptions::default()
        },
    )
    .result()
    .await
    .expect("second result");
    assert_eq!(second.stop_reason, StopReason::Stop);

    let (conn1, _req1, frame1) = backend.next_ws_request().await;
    let (conn2, _req2, frame2) = backend.next_ws_request().await;
    assert_eq!(conn1, 1);
    assert_eq!(conn2, 1, "second request reuses the cached connection");
    assert!(frame1.get("previous_response_id").is_none());
    assert_eq!(frame1["input"].as_array().expect("input").len(), 1);
    assert_eq!(frame2["previous_response_id"], json!(first_id));
    // Delta: only the new user message (assistant replay is covered by the
    // continuation baseline).
    let delta = frame2["input"].as_array().expect("delta input");
    assert_eq!(delta.len(), 1, "delta input: {delta:?}");
    assert_eq!(delta[0]["role"], json!("user"));
    assert_eq!(
        delta[0]["content"][0]["text"],
        json!("Now finish"),
        "delta: {delta:?}"
    );

    let stats = get_openai_codex_websocket_debug_stats(session).expect("stats");
    assert_eq!(stats.requests, 2);
    assert_eq!(stats.connections_created, 1);
    assert_eq!(stats.connections_reused, 1);
    assert_eq!(stats.cached_context_requests, 2);
    assert_eq!(stats.full_context_requests, 1);
    assert_eq!(stats.delta_requests, 1);
    assert_eq!(stats.last_delta_input_items, Some(1));
    assert_eq!(stats.last_previous_response_id.as_deref(), Some("resp_1"));
    assert_eq!(stats.store_true_requests, 0);

    // Third request with a changed system prompt: the baseline check fails
    // and the full context is sent.
    let ctx3 = rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: Some("Different instructions.".to_owned()),
        messages: vec![
            user_text("hi"),
            Message::Assistant(second),
            user_text("third"),
        ],
        tools: None,
    });
    let third = codex_stream(
        &m,
        &ctx3,
        OpenAiCodexResponsesOptions {
            stream: ws_options(Some(session), Transport::WebsocketCached),
            ..OpenAiCodexResponsesOptions::default()
        },
    )
    .result()
    .await
    .expect("third result");
    assert_eq!(third.stop_reason, StopReason::Stop);

    let (_conn3, _req3, frame3) = backend.next_ws_request().await;
    assert!(frame3.get("previous_response_id").is_none());
    assert_eq!(frame3["input"].as_array().expect("full input").len(), 3);
    let stats = get_openai_codex_websocket_debug_stats(session).expect("stats");
    assert_eq!(stats.full_context_requests, 2);
    assert_eq!(stats.delta_requests, 1);

    close_openai_codex_web_socket_sessions(Some(session)).await;
    reset_openai_codex_websocket_debug_stats(Some(session));
    assert_eq!(backend.http_requests.load(Ordering::SeqCst), 0);
}

/// Upstream: "scopes cached websockets to the authenticated account"
/// (cfe6b6a05 @ 4181f66, #7284): rotating accounts must not reuse a socket
/// authenticated by another account; the pool is keyed `sessionId →
/// accountId`.
#[tokio::test]
async fn test_websocket_cache_scoped_to_account() {
    let _guard = WS_TEST_LOCK.lock().await;
    let session = "shared-session";
    reset_openai_codex_websocket_debug_stats(Some(session));

    let (handshake, request, http) = (
        accept_all(),
        ws_hook(|conn, req, _| ws_text_events(&format!("resp_{conn}_{req}"), "Hello")),
        http_fixed(500, "unexpected fetch".to_owned()),
    );
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let ctx = context(vec![]);

    for account in ["account-a", "account-b", "account-a"] {
        let result = codex_stream(
            &m,
            &ctx,
            OpenAiCodexResponsesOptions {
                stream: StreamOptions {
                    transport: Some(Transport::WebsocketCached),
                    session_id: Some(session.to_owned()),
                    request: rpi_ai::ProviderRequestOptions {
                        api_key: Some(mock_token_for(account)),
                        ..Default::default()
                    },
                    ..StreamOptions::default()
                },
                ..OpenAiCodexResponsesOptions::default()
            },
        )
        .result()
        .await
        .expect("result");
        assert_eq!(result.stop_reason, StopReason::Stop);
    }

    let header = |headers: &[(String, String)], name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let (conn1, headers1) = backend.next_ws_open().await;
    let (_c, _r, _frame1) = backend.next_ws_request().await;
    let (conn2, headers2) = backend.next_ws_open().await;
    let (_c, _r, _frame2) = backend.next_ws_request().await;
    // The third request reuses account-a's connection.
    let (conn3, _r, _frame3) = backend.next_ws_request().await;
    assert_eq!((conn1, conn2, conn3), (1, 2, 1));
    assert_eq!(
        header(&headers1, "chatgpt-account-id").as_deref(),
        Some("account-a")
    );
    assert_eq!(
        header(&headers2, "chatgpt-account-id").as_deref(),
        Some("account-b")
    );
    assert_eq!(
        header(&headers1, "authorization").as_deref(),
        Some(format!("Bearer {}", mock_token_for("account-a")).as_str())
    );
    assert_eq!(
        header(&headers2, "authorization").as_deref(),
        Some(format!("Bearer {}", mock_token_for("account-b")).as_str())
    );
    assert_eq!(backend.http_requests.load(Ordering::SeqCst), 0);

    let stats = get_openai_codex_websocket_debug_stats(session).expect("stats");
    assert_eq!(stats.connections_created, 2);
    assert_eq!(stats.connections_reused, 1);

    close_openai_codex_web_socket_sessions(Some(session)).await;
    reset_openai_codex_websocket_debug_stats(Some(session));
}

/// Connection cache TTLs: reuse within the 5min idle TTL; eviction after it
/// (parameterized to milliseconds via the test seam); a connection older than
/// the 55min max age is replaced on the next acquire.
#[tokio::test]
async fn test_websocket_connection_cache_ttls() {
    let _guard = WS_TEST_LOCK.lock().await;
    let session = "ttl-1";
    reset_openai_codex_websocket_debug_stats(Some(session));
    let previous =
        set_codex_websocket_ttls_for_tests(Duration::from_millis(80), Duration::from_secs(10));

    let (handshake, request, http) = (
        accept_all(),
        ws_hook(|_, req, _| ws_text_events(&format!("resp_{req}"), "ok")),
        http_fixed(200, sse_payload("unused")),
    );
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let run = || async {
        codex_stream(
            &m,
            &context(vec![user_text("hi")]),
            OpenAiCodexResponsesOptions {
                stream: ws_options(Some(session), Transport::WebsocketCached),
                ..OpenAiCodexResponsesOptions::default()
            },
        )
        .result()
        .await
    };

    assert!(run().await.is_some());
    assert!(run().await.is_some(), "reuse within idle TTL");
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(run().await.is_some(), "fresh connection after idle TTL");

    for _ in 0..3 {
        let _ = backend.next_ws_request().await;
    }
    let stats = get_openai_codex_websocket_debug_stats(session).expect("stats");
    assert_eq!(stats.connections_reused, 1, "stats: {stats:?}");
    assert_eq!(stats.connections_created, 2, "stats: {stats:?}");

    // Max age: a connection older than the hard limit is closed on the next
    // acquire (upstream "opens a fresh cached websocket before the backend
    // connection age limit").
    let session = "ttl-2";
    reset_openai_codex_websocket_debug_stats(Some(session));
    set_codex_websocket_ttls_for_tests(Duration::from_secs(10), Duration::from_millis(80));
    assert!(run_aged(&m, session).await.is_some());
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(run_aged(&m, session).await.is_some());
    let stats = get_openai_codex_websocket_debug_stats(session).expect("stats");
    assert_eq!(stats.connections_created, 2, "stats: {stats:?}");
    assert_eq!(stats.connections_reused, 0, "stats: {stats:?}");

    set_codex_websocket_ttls_for_tests(previous.0, previous.1);
    close_openai_codex_web_socket_sessions(None).await;
    reset_openai_codex_websocket_debug_stats(None);
}

async fn run_aged(m: &Model, session: &str) -> Option<rpi_ai::types::AssistantMessage> {
    codex_stream(
        m,
        &context(vec![user_text("hi")]),
        OpenAiCodexResponsesOptions {
            stream: ws_options(Some(session), Transport::WebsocketCached),
            ..OpenAiCodexResponsesOptions::default()
        },
    )
    .result()
    .await
}

/// Upstream: "falls back to SSE when a websocket is idle before the first
/// event" + the per-session permanent fallback.
#[tokio::test]
async fn test_websocket_idle_fallback_is_permanent() {
    let _guard = WS_TEST_LOCK.lock().await;
    let session = "fb-1";
    reset_openai_codex_websocket_debug_stats(Some(session));

    // The WS side completes the handshake but never sends an event; the
    // adapter's idle timeout (timeout_ms) fails the attempt before stream
    // start and falls back to SSE — permanently for this session.
    let (handshake, request, http) = (
        accept_all(),
        no_events(),
        http_fixed(200, sse_payload("Hello")),
    );
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));

    let mut options = ws_options(Some(session), Transport::Auto);
    options.timeout_ms = Some(60);
    let events =
        collect(OpenAiCodexResponses.stream(&m, &context(vec![user_text("hi")]), Some(options)))
            .await;
    assert_eq!(event_kinds(&events).last(), Some(&"done"));

    let (_conn, _req, _frame) = backend.next_ws_request().await;
    let http = backend.next_http().await;
    assert_eq!(http.path, "/codex/responses");

    let StreamEvent::Done { message, .. } = events.last().expect("done") else {
        panic!("expected done");
    };
    // The transport failure is recorded as a diagnostic on the message.
    let diagnostics = message.diagnostics.as_ref().expect("diagnostics");
    assert_eq!(diagnostics[0].kind, "provider_transport_failure");
    let details = diagnostics[0].details.as_ref().expect("details");
    assert_eq!(details["configuredTransport"], json!("auto"));
    assert_eq!(details["fallbackTransport"], json!("sse"));
    assert_eq!(details["eventsEmitted"], json!(false));
    assert_eq!(details["phase"], json!("before_message_stream_start"));

    let stats = get_openai_codex_websocket_debug_stats(session).expect("stats");
    assert_eq!(stats.websocket_failures, 1);
    assert_eq!(stats.sse_fallbacks, 1);
    assert_eq!(stats.websocket_fallback_active, Some(true));
    assert_eq!(
        stats.last_web_socket_error.as_deref(),
        Some("WebSocket idle timeout after 60ms")
    );

    // Second request on the same session: WS is skipped entirely.
    let events = collect(OpenAiCodexResponses.stream(
        &m,
        &context(vec![user_text("hi again")]),
        Some(ws_options(Some(session), Transport::Auto)),
    ))
    .await;
    assert_eq!(event_kinds(&events).last(), Some(&"done"));
    let _ = backend.next_http().await;
    assert_eq!(backend.ws_connections.load(Ordering::SeqCst), 1);
    assert_eq!(backend.http_requests.load(Ordering::SeqCst), 2);
    let stats = get_openai_codex_websocket_debug_stats(session).expect("stats");
    assert_eq!(stats.sse_fallbacks, 2);
    // A different session is unaffected and still tries WS first.
    reset_openai_codex_websocket_debug_stats(Some(session));
}

/// Upstream: "reconnects once when the websocket connection limit is reached
/// before output starts".
#[tokio::test]
async fn test_websocket_connection_limit_retry_once() {
    let _guard = WS_TEST_LOCK.lock().await;
    let (handshake, request, http) = (
        accept_all(),
        ws_hook(|conn, _, _| {
            if conn == 1 {
                vec![json!({"type": "error",
                            "error": {"code": "websocket_connection_limit_reached"}})]
            } else {
                ws_text_events("resp_1", "ok")
            }
        }),
        http_fixed(200, sse_payload("unused")),
    );
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let result = OpenAiCodexResponses
        .stream(
            &m,
            &context(vec![user_text("hi")]),
            Some(ws_options(None, Transport::Auto)),
        )
        .result()
        .await
        .expect("result");
    assert_eq!(result.stop_reason, StopReason::Stop);

    let (conn1, _, _) = backend.next_ws_request().await;
    let (conn2, _, _) = backend.next_ws_request().await;
    assert_eq!((conn1, conn2), (1, 2));
    assert_eq!(backend.http_requests.load(Ordering::SeqCst), 0);
}

/// Upstream: "recovers a missing cached websocket continuation" — a
/// `previous_response_not_found` error is retried once with the full context.
#[tokio::test]
async fn test_websocket_previous_response_not_found_retry() {
    let _guard = WS_TEST_LOCK.lock().await;
    let session = "prnf-1";
    reset_openai_codex_websocket_debug_stats(Some(session));

    let (handshake, request, http) = (
        accept_all(),
        ws_hook(|conn, req, body| {
            if body.get("previous_response_id").is_some() {
                vec![json!({"type": "error",
                            "error": {"code": "previous_response_not_found",
                                      "message": "Previous response with id 'resp_1_1' not found."}})]
            } else {
                ws_text_events(&format!("resp_{conn}_{req}"), &format!("text{conn}_{req}"))
            }
        }),
        http_fixed(200, sse_payload("unused")),
    );
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));

    let first = codex_stream(
        &m,
        &context(vec![user_text("hi")]),
        OpenAiCodexResponsesOptions {
            stream: ws_options(Some(session), Transport::WebsocketCached),
            ..OpenAiCodexResponsesOptions::default()
        },
    )
    .result()
    .await
    .expect("first");
    assert_eq!(first.stop_reason, StopReason::Stop);

    let second = codex_stream(
        &m,
        &context(vec![
            user_text("hi"),
            Message::Assistant(first),
            user_text("next"),
        ]),
        OpenAiCodexResponsesOptions {
            stream: ws_options(Some(session), Transport::WebsocketCached),
            ..OpenAiCodexResponsesOptions::default()
        },
    )
    .result()
    .await
    .expect("second");
    assert_eq!(second.stop_reason, StopReason::Stop);
    assert_eq!(second.response_id.as_deref(), Some("resp_2_1"));

    let (_, _, frame1) = backend.next_ws_request().await;
    let (_, _, frame2) = backend.next_ws_request().await;
    let (_, _, frame3) = backend.next_ws_request().await;
    assert!(frame1.get("previous_response_id").is_none());
    // Frame 2 attempts the delta continuation and is rejected...
    assert_eq!(frame2["previous_response_id"], json!("resp_1_1"));
    assert_eq!(frame2["input"].as_array().expect("delta").len(), 1);
    // ...and the retry sends the full context without a previous_response_id.
    assert!(frame3.get("previous_response_id").is_none());
    assert_eq!(frame3["input"].as_array().expect("full").len(), 3);
    assert_eq!(backend.ws_connections.load(Ordering::SeqCst), 2);
    assert_eq!(backend.http_requests.load(Ordering::SeqCst), 0);

    close_openai_codex_web_socket_sessions(Some(session)).await;
    reset_openai_codex_websocket_debug_stats(Some(session));
}

/// Upstream: "closes one-shot websockets when cacheRetention is none".
#[tokio::test]
async fn test_websocket_one_shot_when_cache_retention_none() {
    let _guard = WS_TEST_LOCK.lock().await;
    let session = "one-off-summary";
    reset_openai_codex_websocket_debug_stats(Some(session));

    let (handshake, request, http) = (
        accept_all(),
        ws_hook(|conn, _, _| ws_text_events(&format!("resp_{conn}"), "ok")),
        http_fixed(200, sse_payload("unused")),
    );
    let mut backend = serve_backend(handshake, request, http).await;
    let m = model(&backend.base_url, json!({}));
    let mut options = ws_options(Some(session), Transport::Auto);
    options.cache_retention = Some(CacheRetention::None);

    for _ in 0..2 {
        let result = OpenAiCodexResponses
            .stream(&m, &context(vec![user_text("hi")]), Some(options.clone()))
            .result()
            .await
            .expect("result");
        assert_eq!(result.stop_reason, StopReason::Stop);
    }

    let (conn1, _, frame1) = backend.next_ws_request().await;
    let (conn2, _, frame2) = backend.next_ws_request().await;
    assert_eq!(
        (conn1, conn2),
        (1, 2),
        "each request opens a fresh connection"
    );
    assert!(frame1.get("prompt_cache_key").is_none());
    assert!(frame2.get("prompt_cache_key").is_none());
    // No stats are recorded without a cache session.
    assert!(get_openai_codex_websocket_debug_stats(session).is_none());
    assert_eq!(backend.http_requests.load(Ordering::SeqCst), 0);
}
