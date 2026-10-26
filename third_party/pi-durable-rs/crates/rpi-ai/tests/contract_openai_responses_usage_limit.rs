//! Contract tests for the Sign in with ChatGPT usage-limit handling
//! (`02eed88fd`): both the HTTP-rejection path (`api/openai_responses.rs`
//! request error) and the in-stream `response.failed` path append the
//! ChatGPT usage link to the final error message, and only for the shared
//! usage-limit code. Mirrors `test/openai-responses-usage-limit.test.ts` @
//! pi abe508e1b (v1.1.0) at the adapter level: the upstream test stubs
//! `fetch`, here a scripted loopback server records the request and serves
//! the recorded payloads.

use std::time::Duration;

use futures::StreamExt;
use rpi_ai::api::openai_responses::OpenAiResponses;
use rpi_ai::models::ProviderStreams;
use rpi_ai::types::{ApiKind, Context, Message, Model, StopReason, StreamEvent, StreamOptions};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const USAGE_URL: &str = "https://chatgpt.com/settings/usage";

/// One captured request: request line, headers (lowercased names) and body.
#[derive(Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    body: String,
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
    let mut content_length = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().expect("content-length");
        }
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
        body: String::from_utf8_lossy(&body[..content_length]).to_string(),
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn model(base_url: &str) -> Model {
    serde_json::from_value(json!({
        "id": "gpt-5",
        "name": "GPT-5",
        "api": ApiKind::OPENAI_RESPONSES,
        "provider": "openai",
        "baseUrl": base_url,
        "reasoning": false,
        "input": ["text"],
        "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.5, "cacheWrite": 1.0},
        "contextWindow": 128000,
        "maxTokens": 8192
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

fn options() -> StreamOptions {
    StreamOptions {
        request: rpi_ai::ProviderRequestOptions {
            api_key: Some("chatgpt-access-token".to_owned()),
            ..Default::default()
        },
        ..StreamOptions::default()
    }
}

fn error_message(events: &[StreamEvent]) -> String {
    events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Error { error, .. } => error.error_message.clone(),
            _ => None,
        })
        .expect("error event")
}

/// The HTTP rejection path: a 429 whose body carries the shared usage-limit
/// code reaches the caller with the usage link appended.
#[tokio::test]
async fn http_rejection_appends_the_chatgpt_usage_link() {
    const BODY: &str = r#"{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"The usage limit has been reached"}}"#;
    let (base_url, mut captured) = serve(vec![(429, BODY)]).await;
    let m = model(&base_url);
    let events: Vec<StreamEvent> = tokio::time::timeout(
        Duration::from_secs(10),
        OpenAiResponses
            .stream(&m, &context(), Some(options()))
            .collect(),
    )
    .await
    .expect("stream completes");

    let request = captured.recv().await.expect("request captured");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/responses");
    let body: Value = serde_json::from_str(&request.body).expect("request body is JSON");
    assert_eq!(body["model"], json!("gpt-5"));
    assert_eq!(body["stream"], json!(true));

    let message = error_message(&events);
    assert!(
        message.contains("subscription_sharing_usage_limit_exceeded"),
        "final message keeps the provider code: {message}"
    );
    assert!(
        message.ends_with(&format!("\nCheck your ChatGPT usage: {USAGE_URL}")),
        "final message carries the usage link: {message}"
    );
    let error = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Error { error, .. } => Some(error),
            _ => None,
        })
        .expect("error event");
    assert_eq!(error.stop_reason, StopReason::Error);
}

/// The in-stream `response.failed` path (the code arrives mid-stream without
/// an HTTP error) appends the same link.
#[tokio::test]
async fn stream_response_failed_appends_the_chatgpt_usage_link() {
    const FAILED_SSE: &str = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n",
        "\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_1\",\"status\":\"failed\",\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\",\"message\":\"The usage limit has been reached\"}}}\n",
        "\n",
    );
    let (base_url, _captured) = serve(vec![(200, FAILED_SSE)]).await;
    let m = model(&base_url);
    let events: Vec<StreamEvent> = tokio::time::timeout(
        Duration::from_secs(10),
        OpenAiResponses
            .stream(&m, &context(), Some(options()))
            .collect(),
    )
    .await
    .expect("stream completes");

    let message = error_message(&events);
    assert!(
        message.contains("subscription_sharing_usage_limit_exceeded"),
        "final message keeps the provider code: {message}"
    );
    assert!(
        message.ends_with(&format!("\nCheck your ChatGPT usage: {USAGE_URL}")),
        "final message carries the usage link: {message}"
    );
}

/// Other error codes are untouched.
#[tokio::test]
async fn unrelated_errors_do_not_get_the_chatgpt_usage_link() {
    const BODY: &str = r#"{"error":{"code":"server_error","message":"boom"}}"#;
    let (base_url, _captured) = serve(vec![(429, BODY)]).await;
    let m = model(&base_url);
    let events: Vec<StreamEvent> = tokio::time::timeout(
        Duration::from_secs(10),
        OpenAiResponses
            .stream(&m, &context(), Some(options()))
            .collect(),
    )
    .await
    .expect("stream completes");

    let message = error_message(&events);
    assert!(!message.contains(USAGE_URL), "no hint: {message}");
}
