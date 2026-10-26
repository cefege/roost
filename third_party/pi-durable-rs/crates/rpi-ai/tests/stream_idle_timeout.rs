//! Idle-timeout regression tests for provider streaming (loopback only, no
//! live network access).
//!
//! The original bug: `StreamOptions::timeout_ms` (upstream undici
//! `headersTimeout`/`bodyTimeout`, i.e. **idle** budgets) was mapped to
//! `reqwest::ClientBuilder::timeout` — a **total** deadline covering every
//! phase (connect, headers, and the entire streamed body). Any inference
//! streaming longer than the budget (default 5 min) was killed mid-stream
//! even while actively receiving chunks.
//!
//! These tests drive the full `anthropic_messages` adapter (reqwest default
//! transport) against a loopback SSE server and pin the corrected semantics:
//!
//! 1. an actively streaming response whose TOTAL duration exceeds the budget
//!    must complete successfully;
//! 2. a response that goes silent mid-stream must die with an idle-timeout
//!    error at the budget;
//! 3. a server that never answers with headers must die with a
//!    headers-timeout error at the budget.

use std::time::Duration;

use futures::StreamExt;
use rpi_ai::types::{
    Context, Model, ProviderRequestOptions, SimpleStreamOptions, StreamEvent, StreamOptions,
};
use rpi_ai::utils::event_stream::AssistantMessageEventStream;
use serde_json::json;

/// One scripted server action: sleep `delay_ms`, then write `payload`
/// (`None` = keep the connection open forever, i.e. stall).
type Step = (u64, Option<String>);

/// Spawns a loopback SSE server that waits `head_delay_ms` before sending
/// the response head, then runs `script`; returns its base URL.
async fn sse_server(head_delay_ms: u64, script: Vec<Step>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let script = script.clone();
            let head_delay_ms = head_delay_ms;
            tokio::spawn(async move {
                let mut socket = socket;
                // Drain the request head so the client write side completes.
                let mut buf = vec![0u8; 8192];
                use tokio::io::AsyncReadExt;
                let _ = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                                    return;
                                }
                            }
                        }
                    }
                })
                .await;
                use tokio::io::AsyncWriteExt;
                if head_delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(head_delay_ms)).await;
                }
                let head = concat!(
                    "HTTP/1.1 200 OK\r\n",
                    "content-type: text/event-stream\r\n",
                    "cache-control: no-cache\r\n",
                    "connection: close\r\n",
                    "\r\n",
                );
                if socket.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for (delay_ms, payload) in script {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    if payload.is_none() {
                        // Stall: hold the socket open without further data.
                        std::future::pending::<()>().await;
                    }
                    if socket
                        .write_all(payload.expect("checked").as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// A minimal complete Anthropic SSE exchange, split into separately-timed
/// chunks (mirrors the lib-test recorded stream, text-only).
fn sse_script() -> Vec<Step> {
    let event = |name: &str, data: &str| Some(format!("event: {name}\ndata: {data}\n\n"));
    vec![
        (
            0,
            event(
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_123","usage":{"input_tokens":10,"output_tokens":1}}}"#,
            ),
        ),
        (
            60,
            event(
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
        ),
        (
            60,
            event(
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
            ),
        ),
        (
            60,
            event(
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
        ),
        (
            60,
            event(
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":25}}"#,
            ),
        ),
        (60, event("message_stop", r#"{"type":"message_stop"}"#)),
    ]
}

fn model(base_url: &str) -> Model {
    let value = json!({
        "id": "claude-sonnet-4-5", "name": "Sonnet", "api": "anthropic-messages",
        "provider": "anthropic", "baseUrl": base_url,
        "reasoning": false, "input": ["text"],
        "cost": {"input": 3.0, "output": 15.0, "cacheRead": 0.3, "cacheWrite": 3.75},
        "contextWindow": 200000, "maxTokens": 64000
    });
    serde_json::from_value(value).expect("model")
}

fn context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![
            serde_json::from_value(json!({"role": "user", "content": "hello", "timestamp": 1}))
                .expect("user"),
        ],
        tools: None,
    }
}

fn options(timeout_ms: u64) -> SimpleStreamOptions {
    SimpleStreamOptions {
        stream: StreamOptions {
            request: ProviderRequestOptions {
                api_key: Some("test-key".to_owned()),
                timeout_ms: Some(timeout_ms),
                max_retries: Some(0),
                ..Default::default()
            },
            ..StreamOptions::default()
        },
        reasoning: None,
        thinking_budgets: None,
        tool_choice: None,
    }
}

async fn collect(stream: AssistantMessageEventStream) -> Vec<StreamEvent> {
    tokio::time::timeout(Duration::from_secs(30), stream.collect())
        .await
        .expect("stream completes within 30s")
}

fn terminal(events: &[StreamEvent]) -> (&'static str, Option<String>) {
    for event in events.iter().rev() {
        match event {
            StreamEvent::Error { error, .. } => return ("error", error.error_message.clone()),
            StreamEvent::Done { message, .. } => return ("done", message.error_message.clone()),
            _ => {}
        }
    }
    panic!("expected a terminal event, got {events:?}")
}

/// Regression: an actively streaming response whose total duration (~360ms)
/// exceeds the timeout budget (250ms) must complete successfully — only a
/// *silent* connection may be terminated by the budget.
#[tokio::test]
async fn active_stream_longer_than_timeout_completes() {
    let script = sse_script(); // 5 gaps x 60ms ≈ 300ms total after headers
    let url = sse_server(0, script).await;
    let events = collect(
        rpi_ai::api::anthropic_messages::stream_simple(
            &model(&url),
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(options(250)),
        )
        .expect("stream_simple"),
    )
    .await;

    let (kind, error) = terminal(&events);
    assert_eq!(kind, "done", "stream must complete, error: {error:?}");
    assert!(error.is_none(), "no error expected: {error:?}");
}

/// A response that starts streaming and then goes silent dies with an
/// idle-timeout error at the budget (upstream `bodyTimeout`).
#[tokio::test]
async fn silent_stream_dies_at_idle_timeout() {
    let mut script = sse_script();
    // Keep only the start, then stall forever (no further bytes, socket
    // held open).
    script.truncate(1);
    script.push((0, None));
    let url = sse_server(0, script).await;
    let events = collect(
        rpi_ai::api::anthropic_messages::stream_simple(
            &model(&url),
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(options(200)),
        )
        .expect("stream_simple"),
    )
    .await;

    let (kind, error) = terminal(&events);
    let error = error.expect("terminal error expected");
    assert_eq!(kind, "error");
    assert!(
        error.to_lowercase().contains("timeout"),
        "idle-timeout message expected: {error}"
    );
}

/// A server that never answers with headers dies at the budget (upstream
/// `headersTimeout`).
#[tokio::test]
async fn headers_wait_is_bounded() {
    // The server delays the response HEAD itself: no status line, no
    // headers, within the budget.
    let url = sse_server(60_000, vec![]).await;
    let events = collect(
        rpi_ai::api::anthropic_messages::stream_simple(
            &model(&url),
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(options(150)),
        )
        .expect("stream_simple"),
    )
    .await;

    let (kind, error) = terminal(&events);
    let error = error.expect("terminal error expected");
    assert_eq!(kind, "error");
    assert!(
        error.contains("waiting for response headers"),
        "headers-timeout message expected: {error}"
    );
}

/// Sanity: with the budget disabled (`None`), the adapter performs no idle
/// enforcement at all (transparent passthrough).
#[tokio::test]
async fn no_timeout_option_disables_enforcement() {
    let script = sse_script();
    let url = sse_server(0, script).await;
    let events = collect(
        rpi_ai::api::anthropic_messages::stream_simple(
            &model(&url),
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(SimpleStreamOptions {
                stream: StreamOptions {
                    request: ProviderRequestOptions {
                        api_key: Some("test-key".to_owned()),
                        max_retries: Some(0),
                        ..Default::default()
                    },
                    ..StreamOptions::default()
                },
                reasoning: None,
                thinking_budgets: None,
                tool_choice: None,
            }),
        )
        .expect("stream_simple"),
    )
    .await;
    let (kind, error) = terminal(&events);
    assert_eq!(kind, "done", "stream must complete, error: {error:?}");
}

#[test]
fn sse_script_total_duration_exceeds_budgets() {
    // Guard the guard: the active-stream test's script must genuinely run
    // longer than its timeout budget, or the regression test proves nothing.
    let total: u64 = sse_script().iter().map(|(delay, _)| delay).sum();
    assert!(total >= 300, "script total {total}ms must exceed 250ms");
}
