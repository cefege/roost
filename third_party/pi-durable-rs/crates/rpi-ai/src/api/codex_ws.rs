//! Codex WebSocket subsystem: connection state machine and session cache for
//! `openai-codex-responses` (port of the WebSocket half of
//! `packages/ai/src/api/openai-codex-responses.ts` @ pi 0.82.1 (2efa728)).
//!
//! ## State machine (design §13, finalized here)
//!
//! ```text
//!                 connect (handshake, connect_timeout default 15s)
//!   Disconnected ───────────────────────────────────────────────► Open
//!                                                                     │ acquire
//!                                                                     ▼
//!   Closed ◄── error/abort ── Busy (in-flight response.create) ───────┘
//!     ▲                         │ terminal response event
//!     │                         ▼ (keep=true, reusable)
//!     │                        Idle (cached; 5min TTL timer, SESSION_WEBSOCKET_CACHE_TTL_MS)
//!     │                         │ acquire (reuse)                │ TTL fires while idle
//!     │                         ▼                                ▼
//!     └──────────────────────── Busy                          Closed (evicted)
//!     ▲
//!     └── any entry aged ≥ 55min (SESSION_WEBSOCKET_MAX_AGE_MS) is closed on
//!         next acquire and replaced by a fresh connection
//!
//!   Session-level fallback (one-way): a transport failure before the first
//!   stream event records a permanent per-session SSE fallback; later requests
//!   for that session skip WebSocket entirely. A failure after stream start
//!   propagates as an error (no fallback).
//!
//!   One-shot retries inside the WebSocket phase:
//!   - `websocket_connection_limit_reached` before stream start: reconnect once
//!   - `previous_response_not_found`: retry once (continuation was cleared, so
//!     the retry sends the full context)
//! ```
//!
//! Differences from the upstream event-listener expression (all registered in
//! the D-027 deviation family): the socket is moved out of the cache entry
//! while `busy` (upstream flips a `busy` boolean on a shared object); idle
//! expiry is a spawned timer task guarded by a generation counter (upstream
//! `setTimeout` + `clearTimeout`); cached-socket reusability is probed with a
//! non-blocking poll instead of `readyState`, stashing a surprisingly-arrived
//! message for the next read.

pub mod cache;

use std::time::Duration;

use futures::StreamExt;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

pub use cache::{
    CachedWebSocketContinuationState, OpenAiCodexWebSocketDebugStats,
    close_openai_codex_web_socket_sessions, get_openai_codex_websocket_debug_stats,
    record_websocket_failure, record_websocket_sse_fallback,
    reset_openai_codex_websocket_debug_stats, set_codex_websocket_ttls_for_tests,
};

/// `DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS`.
pub const DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS: u64 = 15_000;
/// `WEBSOCKET_MESSAGE_TOO_BIG_CLOSE_CODE`.
pub const WEBSOCKET_MESSAGE_TOO_BIG_CLOSE_CODE: u16 = 1009;
/// `WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE`.
pub const WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE: &str = "websocket_connection_limit_reached";
/// `PREVIOUS_RESPONSE_NOT_FOUND_CODE`.
pub const PREVIOUS_RESPONSE_NOT_FOUND_CODE: &str = "previous_response_not_found";

/// The tungstenite stream type produced by `connect_async`.
pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ---------------------------------------------------------------------------
// Error taxonomy
// ---------------------------------------------------------------------------

/// Error taxonomy of the Codex adapter. The variants mirror the upstream
/// `Error` subclasses because the retry/fallback control flow classifies on
/// them (`CodexApiError` / `CodexProtocolError` are "non-transport" and skip
/// the SSE fallback; close/connect/idle failures are transport).
#[derive(Debug)]
pub enum CodexError {
    /// `CodexApiError`: server-sent `error` / `response.failed` events.
    Api {
        message: String,
        code: Option<String>,
    },
    /// `CodexProtocolError`: invalid JSON on an SSE event or WebSocket frame.
    Protocol(String),
    /// `WebSocketCloseError`.
    Close {
        message: String,
        code: Option<u16>,
        reason: Option<String>,
    },
    /// Transport failures: connect/handshake/idle-timeout/network.
    Transport(String),
    /// `RetryDelayExceededError` (server asked for a wait above the cap).
    RetryDelayExceeded(String),
    /// `AbortSignal` cancellation ("Request was aborted").
    Aborted,
    /// Internal sentinel: a retry was scheduled inside the SSE attempt
    /// (upstream expresses this with `continue` inside the try block).
    RetryScheduled,
    /// Anything upstream would surface as a plain `Error`.
    Other(String),
}

impl CodexError {
    /// The upstream `Error.message`.
    pub fn message(&self) -> String {
        match self {
            CodexError::Api { message, .. }
            | CodexError::Protocol(message)
            | CodexError::Close { message, .. }
            | CodexError::Transport(message)
            | CodexError::RetryDelayExceeded(message)
            | CodexError::Other(message) => message.clone(),
            CodexError::Aborted => "Request was aborted".to_owned(),
            // invariant: the sentinel is handled before any message read
            CodexError::RetryScheduled => "retry scheduled".to_owned(),
        }
    }

    /// The upstream `Error.name` (diagnostic payloads).
    pub fn error_name(&self) -> &'static str {
        match self {
            CodexError::Api { .. } => "CodexApiError",
            CodexError::Protocol(_) => "CodexProtocolError",
            CodexError::Close { .. } => "WebSocketCloseError",
            CodexError::RetryDelayExceeded(_) => "RetryDelayExceededError",
            _ => "Error",
        }
    }

    /// `isCodexNonTransportError`.
    pub fn is_non_transport(&self) -> bool {
        matches!(self, CodexError::Api { .. } | CodexError::Protocol(_))
    }

    /// `isWebSocketConnectionLimitReachedError`.
    pub fn is_connection_limit_reached(&self) -> bool {
        matches!(
            self,
            CodexError::Api { code: Some(code), .. } if code == WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE
        )
    }

    /// `isPreviousResponseNotFoundError`.
    pub fn is_previous_response_not_found(&self) -> bool {
        matches!(
            self,
            CodexError::Api { code: Some(code), .. } if code == PREVIOUS_RESPONSE_NOT_FOUND_CODE
        )
    }
}

// ---------------------------------------------------------------------------
// Connect
// ---------------------------------------------------------------------------

/// `connectWebSocket`: handshake with the Codex WebSocket endpoint, bounded by
/// the connect timeout (default 15s) and the abort signal.
pub async fn connect(
    url: &str,
    headers: &reqwest::header::HeaderMap,
    connect_timeout_ms: Option<u64>,
    signal: Option<&CancellationToken>,
) -> Result<WsStream, CodexError> {
    let mut request = url
        .into_client_request()
        .map_err(|error| CodexError::Transport(error.to_string()))?;
    for (name, value) in headers {
        request.headers_mut().append(name.clone(), value.clone());
    }

    let handshake = tokio_tungstenite::connect_async(request);
    let with_signal = async {
        match signal {
            Some(signal) => {
                tokio::select! {
                    outcome = handshake => outcome.map_err(|error| CodexError::Transport(error.to_string())),
                    () = signal.cancelled() => Err(CodexError::Aborted),
                }
            }
            None => handshake
                .await
                .map_err(|error| CodexError::Transport(error.to_string())),
        }
    };

    let timeout_ms = connect_timeout_ms.unwrap_or(DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS);
    let outcome = if timeout_ms > 0 {
        match tokio::time::timeout(Duration::from_millis(timeout_ms), with_signal).await {
            Ok(outcome) => outcome,
            Err(_) => {
                return Err(CodexError::Transport(format!(
                    "WebSocket connect timeout after {timeout_ms}ms"
                )));
            }
        }
    } else {
        with_signal.await
    };
    outcome.map(|(socket, _response)| socket)
}

// ---------------------------------------------------------------------------
// Request frame + read loop
// ---------------------------------------------------------------------------

/// Sends the request frame: `JSON.stringify({ type: "response.create", ...requestBody })`.
pub async fn send_request(socket: &mut WsStream, body: &Value) -> Result<(), CodexError> {
    use futures::SinkExt;
    let mut frame = serde_json::Map::new();
    frame.insert("type".to_owned(), Value::from("response.create"));
    if let Value::Object(map) = body {
        frame.extend(map.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    socket
        .send(Message::Text(Value::Object(frame).to_string().into()))
        .await
        .map_err(|error| CodexError::Transport(error.to_string()))
}

/// Why [`read_events`] stopped.
#[derive(Debug)]
pub struct ReadOutcome {
    /// A terminal response event (`response.completed` / `.done` /
    /// `.incomplete`) was seen — the only success exit.
    pub saw_completion: bool,
}

/// `extractWebSocketCloseError`.
fn close_error(frame: Option<&tokio_tungstenite::tungstenite::protocol::CloseFrame>) -> CodexError {
    let Some(frame) = frame else {
        return CodexError::Other("WebSocket closed".to_owned());
    };
    let code: u16 = frame.code.into();
    let reason = frame.reason.as_str();
    let code_text = format!(" {code}");
    let mut reason_text = if reason.is_empty() {
        String::new()
    } else {
        format!(" {reason}")
    };
    if reason_text.is_empty() && code == WEBSOCKET_MESSAGE_TOO_BIG_CLOSE_CODE {
        reason_text = " message too big".to_owned();
    }
    CodexError::Close {
        message: format!("WebSocket closed{code_text}{reason_text}")
            .trim()
            .to_owned(),
        code: Some(code),
        reason: (!reason.is_empty()).then(|| reason.to_owned()),
    }
}

/// Drives the upstream `parseWebSocket` loop: each incoming text/binary frame
/// is JSON-parsed and handed to `on_event`, which returns whether the event
/// was terminal. The loop ends after the terminal event (upstream stops the
/// generator there), on a close frame, on error, on idle timeout, or on abort.
///
/// `pending` is a message stashed by the cache liveness probe; it is processed
/// before polling the socket.
pub async fn read_events(
    socket: &mut WsStream,
    pending: Option<Message>,
    signal: Option<&CancellationToken>,
    idle_timeout_ms: Option<u64>,
    on_event: &mut (dyn FnMut(Value) -> Result<bool, CodexError> + Send),
) -> Result<ReadOutcome, CodexError> {
    let mut pending = pending;
    let mut saw_completion = false;

    loop {
        if signal.is_some_and(|signal| signal.is_cancelled()) {
            return Err(CodexError::Aborted);
        }

        let message = match pending.take() {
            Some(message) => Some(Ok(message)),
            None => {
                let next = async {
                    match signal {
                        Some(signal) => {
                            tokio::select! {
                                message = socket.next() => Poll::Message(message),
                                () = signal.cancelled() => Poll::Aborted,
                            }
                        }
                        None => Poll::Message(socket.next().await),
                    }
                };
                match idle_timeout_ms {
                    Some(ms) if ms > 0 => {
                        match tokio::time::timeout(Duration::from_millis(ms), next).await {
                            Ok(Poll::Message(message)) => message,
                            Ok(Poll::Aborted) => return Err(CodexError::Aborted),
                            Err(_) => {
                                close_silently(socket, 1000, "idle_timeout").await;
                                return Err(CodexError::Transport(format!(
                                    "WebSocket idle timeout after {ms}ms"
                                )));
                            }
                        }
                    }
                    _ => match next.await {
                        Poll::Message(message) => message,
                        Poll::Aborted => return Err(CodexError::Aborted),
                    },
                }
            }
        };

        match message {
            Some(Ok(Message::Text(text))) => {
                let parsed: Value = serde_json::from_str(&text).map_err(|error| {
                    CodexError::Protocol(format!("Invalid Codex WebSocket JSON: {error}"))
                })?;
                if on_event(parsed)? {
                    saw_completion = true;
                    break;
                }
            }
            Some(Ok(Message::Binary(bytes))) => {
                let text = String::from_utf8_lossy(&bytes);
                let parsed: Value = serde_json::from_str(&text).map_err(|error| {
                    CodexError::Protocol(format!("Invalid Codex WebSocket JSON: {error}"))
                })?;
                if on_event(parsed)? {
                    saw_completion = true;
                    break;
                }
            }
            Some(Ok(Message::Ping(payload))) => {
                // tungstenite queues an automatic pong on read; an explicit
                // best-effort pong keeps idle connections alive on runtimes
                // where the queued frame is not flushed without a write.
                use futures::SinkExt;
                let _ = socket.send(Message::Pong(payload)).await;
            }
            Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
            Some(Ok(Message::Close(frame))) => {
                if saw_completion {
                    break;
                }
                return Err(close_error(frame.as_ref()));
            }
            Some(Err(error)) => {
                return Err(CodexError::Transport(error.to_string()));
            }
            None => {
                if saw_completion {
                    break;
                }
                return Err(CodexError::Other(
                    "WebSocket stream closed before response.completed".to_owned(),
                ));
            }
        }
    }

    if !saw_completion {
        return Err(CodexError::Other(
            "WebSocket stream closed before response.completed".to_owned(),
        ));
    }
    Ok(ReadOutcome { saw_completion })
}

enum Poll {
    Message(Option<Result<Message, tokio_tungstenite::tungstenite::Error>>),
    Aborted,
}

/// `closeWebSocketSilently`: best-effort close frame; errors are swallowed.
pub async fn close_silently(socket: &mut WsStream, code: u16, reason: &str) {
    use futures::SinkExt;
    let _ = socket
        .send(Message::Close(Some(
            tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: code.into(),
                reason: reason.into(),
            },
        )))
        .await;
}
