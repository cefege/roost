//! Contract tests for Anthropic workload identity federation (`a9424cd43`)
//! at the anthropic-messages adapter and Models layers. Mirrors
//! `packages/ai/test/anthropic-federation.test.ts` /
//! `anthropic-federation-sdk.test.ts` @ pi abe508e1b (v1.1.0): the mocked
//! SDK fetch becomes a scripted loopback server that serves the token
//! endpoint and the Messages SSE stream, and captures every request.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rpi_ai::api::anthropic_messages::AnthropicMessages;
use rpi_ai::auth::AuthContext;
use rpi_ai::models::{CreateModelsOptions, ProviderStreams, create_models};
use rpi_ai::providers::anthropic::anthropic_provider;
use rpi_ai::types::{
    ApiKind, Context, Message, Model, ProviderEnv, ProviderRequestOptions, StreamEvent,
    StreamOptions,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const ANTHROPIC_SSE: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":2,\"cache_creation_input_tokens\":3}}}\n",
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
                401 => "Unauthorized",
                _ => "Status",
            };
            let content_type = if status == 200 && body.contains("event:") {
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

fn model(provider: &str, base_url: &str) -> Model {
    serde_json::from_value(json!({
        "id": "claude-test",
        "name": "Claude Test",
        "api": ApiKind::ANTHROPIC_MESSAGES,
        "provider": provider,
        "baseUrl": base_url,
        "reasoning": false,
        "input": ["text"],
        "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.5, "cacheWrite": 1.0},
        "contextWindow": 100000,
        "maxTokens": 4096
    }))
    .expect("model")
}

fn context() -> rpi_ai::types::TranscriptContext {
    let user: Message =
        serde_json::from_value(json!({"role": "user", "content": "hi", "timestamp": 0}))
            .expect("user");
    rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: None,
        messages: vec![user],
        tools: None,
    })
}

/// Temp identity-token file removed on drop.
struct TempIdentityFile {
    path: std::path::PathBuf,
}

impl TempIdentityFile {
    fn new() -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rpi-federation-contract-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::write(&path, "header.payload.signature").expect("write identity token");
        Self { path }
    }

    fn path_str(&self) -> String {
        self.path.display().to_string()
    }
}

impl Drop for TempIdentityFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn federation_env(identity_file: &str) -> ProviderEnv {
    let mut env = ProviderEnv::new();
    env.insert(
        "ANTHROPIC_FEDERATION_RULE_ID".to_owned(),
        "fdrl_test".to_owned(),
    );
    env.insert(
        "ANTHROPIC_ORGANIZATION_ID".to_owned(),
        "org-test".to_owned(),
    );
    env.insert(
        "ANTHROPIC_IDENTITY_TOKEN_FILE".to_owned(),
        identity_file.to_owned(),
    );
    env
}

fn federation_options(identity_file: &str) -> StreamOptions {
    StreamOptions {
        request: ProviderRequestOptions {
            env: Some(federation_env(identity_file)),
            ..Default::default()
        },
        ..StreamOptions::default()
    }
}

async fn collect(
    stream: rpi_ai::utils::event_stream::AssistantMessageEventStream,
) -> Vec<StreamEvent> {
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.collect::<Vec<StreamEvent>>(),
    )
    .await
    .expect("stream completes")
}

/// The exchange mints the access token and the Messages request carries it as
/// a bearer plus the OAuth beta header, with no API key (`hands the SDK a
/// federation config instead of a key`).
#[tokio::test]
async fn federation_mints_a_bearer_token_and_oauth_beta_header() {
    let identity = TempIdentityFile::new();
    let script = vec![
        (
            200,
            "{\"access_token\":\"federated-token\",\"expires_in\":3600}",
        ),
        (200, ANTHROPIC_SSE),
    ];
    let (base_url, mut captured) = serve(script).await;
    let m = model("anthropic", &base_url);
    let events = collect(AnthropicMessages.stream(
        &m,
        &context(),
        Some(federation_options(&identity.path_str())),
    ))
    .await;

    let exchange = captured.recv().await.expect("token exchange");
    assert_eq!(exchange.method, "POST");
    assert_eq!(exchange.path, "/v1/oauth/token");
    let body = exchange.body_json();
    assert_eq!(
        body["grant_type"],
        json!("urn:ietf:params:oauth:grant-type:jwt-bearer")
    );
    assert_eq!(body["assertion"], json!("header.payload.signature"));
    assert_eq!(body["federation_rule_id"], json!("fdrl_test"));
    assert_eq!(body["organization_id"], json!("org-test"));
    assert_eq!(
        exchange.header("anthropic-beta"),
        Some("oauth-2025-04-20,oidc-federation-2026-04-01")
    );

    let messages = captured.recv().await.expect("messages request");
    assert_eq!(messages.method, "POST");
    assert_eq!(messages.path, "/v1/messages");
    assert_eq!(
        messages.header("authorization"),
        Some("Bearer federated-token")
    );
    assert!(
        messages
            .header("anthropic-beta")
            .is_some_and(|value| value.contains("oauth-2025-04-20")),
        "federated API request appends the OAuth beta: {:?}",
        messages.header("anthropic-beta")
    );
    assert!(messages.header("x-api-key").is_none());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Error { .. }))
    );
}

/// `does not run the SDK credential chain for header-owned auth`.
#[tokio::test]
async fn header_owned_auth_does_not_run_the_credential_chain() {
    let identity = TempIdentityFile::new();
    let (base_url, mut captured) = serve(vec![(200, ANTHROPIC_SSE)]).await;
    let m = model("anthropic", &base_url);
    let mut options = federation_options(&identity.path_str());
    let mut headers = rpi_ai::types::ProviderHeaders::new();
    headers.insert(
        "Authorization".to_owned(),
        Some("Bearer auth-token".to_owned()),
    );
    options.request.headers = Some(headers);
    let events = collect(AnthropicMessages.stream(&m, &context(), Some(options))).await;

    let request = captured.recv().await.expect("messages request");
    assert_eq!(request.path, "/v1/messages");
    assert_eq!(request.header("authorization"), Some("Bearer auth-token"));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Error { .. }))
    );
}

/// `lets an explicit API key win over federation env`.
#[tokio::test]
async fn explicit_api_key_wins_over_federation_env() {
    let identity = TempIdentityFile::new();
    let (base_url, mut captured) = serve(vec![(200, ANTHROPIC_SSE)]).await;
    let m = model("anthropic", &base_url);
    let mut options = federation_options(&identity.path_str());
    options.request.api_key = Some("sk-ant-api03-explicit".to_owned());
    let events = collect(AnthropicMessages.stream(&m, &context(), Some(options))).await;

    let request = captured.recv().await.expect("messages request");
    assert_eq!(request.path, "/v1/messages");
    assert_eq!(request.header("x-api-key"), Some("sk-ant-api03-explicit"));
    assert!(request.header("authorization").is_none());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Error { .. }))
    );
}

/// `does not federate other anthropic-messages providers`.
#[tokio::test]
async fn other_anthropic_messages_providers_do_not_federate() {
    let identity = TempIdentityFile::new();
    let (base_url, _captured) = serve(vec![]).await;
    let m = model("kimi-coding", &base_url);
    let events = collect(AnthropicMessages.stream(
        &m,
        &context(),
        Some(federation_options(&identity.path_str())),
    ))
    .await;
    let message = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Error { error, .. } => error.error_message.clone(),
            _ => None,
        })
        .expect("error event");
    assert!(
        message.contains("No API key for provider: kimi-coding"),
        "message: {message}"
    );
}

/// The SDK's reactive refresh: a 401 from the Messages request invalidates
/// the cache, re-exchanges, and retries once.
#[tokio::test]
async fn reactive_refresh_retries_once_on_401() {
    let identity = TempIdentityFile::new();
    let script = vec![
        (200, "{\"access_token\":\"token-1\",\"expires_in\":3600}"),
        (
            401,
            "{\"error\":{\"type\":\"authentication_error\",\"message\":\"expired\"}}",
        ),
        (200, "{\"access_token\":\"token-2\",\"expires_in\":3600}"),
        (200, ANTHROPIC_SSE),
    ];
    let (base_url, mut captured) = serve(script).await;
    let m = model("anthropic", &base_url);
    let events = collect(AnthropicMessages.stream(
        &m,
        &context(),
        Some(federation_options(&identity.path_str())),
    ))
    .await;

    let first_exchange = captured.recv().await.expect("first exchange");
    assert_eq!(first_exchange.path, "/v1/oauth/token");
    let first_messages = captured.recv().await.expect("first messages");
    assert_eq!(first_messages.path, "/v1/messages");
    assert_eq!(
        first_messages.header("authorization"),
        Some("Bearer token-1")
    );
    let second_exchange = captured.recv().await.expect("second exchange");
    assert_eq!(second_exchange.path, "/v1/oauth/token");
    let retried_messages = captured.recv().await.expect("retried messages");
    assert_eq!(retried_messages.path, "/v1/messages");
    assert_eq!(
        retried_messages.header("authorization"),
        Some("Bearer token-2")
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Error { .. })),
        "the retry succeeds"
    );
}

/// `threads authContext federation variables through Models`: the resolved
/// env reaches the adapter through `Models.streamSimple`.
#[tokio::test]
async fn models_stream_simple_threads_federation_env() {
    struct MapAuthContext(ProviderEnv);
    #[async_trait::async_trait]
    impl AuthContext for MapAuthContext {
        async fn env(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }

        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    let identity = TempIdentityFile::new();
    let (base_url, mut captured) = serve(vec![
        (
            200,
            "{\"access_token\":\"federated-token\",\"expires_in\":3600}",
        ),
        (200, ANTHROPIC_SSE),
    ])
    .await;
    let models = create_models(Some(CreateModelsOptions {
        credentials: None,
        auth_context: Some(Arc::new(MapAuthContext(federation_env(
            &identity.path_str(),
        )))),
        models_store: None,
    }));
    models.set_provider(anthropic_provider());
    let m = model("anthropic", &base_url);
    let events = collect(models.stream_simple(&m, &Context::default(), None)).await;

    let exchange = captured.recv().await.expect("token exchange");
    assert_eq!(exchange.path, "/v1/oauth/token");
    let request = captured.recv().await.expect("messages request");
    assert_eq!(request.path, "/v1/messages");
    assert_eq!(
        request.header("authorization"),
        Some("Bearer federated-token")
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Error { .. }))
    );
}

/// The federation suffix stays scoped: a stored key resolves normally even
/// when the federation env is present (covered here through `Models` for the
/// `streamSimple` path).
#[tokio::test]
async fn stored_credentials_still_resolve_with_federation_env_present() {
    let identity = TempIdentityFile::new();
    let (base_url, mut captured) = serve(vec![(200, ANTHROPIC_SSE)]).await;
    let m = model("anthropic", &base_url);
    let mut options = federation_options(&identity.path_str());
    options.request.api_key = Some("sk-ant-api03-stored".to_owned());
    let _ = collect(AnthropicMessages.stream(&m, &context(), Some(options))).await;
    let request = captured.recv().await.expect("messages request");
    assert_eq!(request.header("x-api-key"), Some("sk-ant-api03-stored"));
    assert!(request.header("authorization").is_none());
}
