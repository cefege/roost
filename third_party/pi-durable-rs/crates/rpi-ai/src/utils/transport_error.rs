//! Normalization of `reqwest` transport failures into the upstream SDK error
//! wording (`Request timed out.` / `Connection error.`), with the
//! `std::error::Error::source` chain appended.
//!
//! `reqwest::Error`'s `Display` prints only the top-level kind plus the URL
//! (`error sending request for url (...)`) and drops the underlying cause.
//! The retry classifier
//! ([`crate::utils::retry::is_retryable_assistant_error`]) is a port of the
//! upstream regex table, which was written against the Node SDK messages
//! (`Connection error.`, `Request timed out.`, `getaddrinfo ENOTFOUND`,
//! `fetch failed`, ...). Without this normalization, connect timeouts,
//! connection refusals, DNS failures, resets, and mid-stream body drops
//! classify as non-retryable and the agent-level retry (upstream
//! `retry.maxRetries`, default 3) never fires.
//!
//! Mapping (mirrors the OpenAI / Anthropic SDK exception shapes, verified in
//! `external/pi` `node_modules/openai/src/core/error.ts:110-121` and
//! `node_modules/@anthropic-ai/sdk/core/error.mjs:73-82`):
//! - `is_timeout()` → `Request timed out.` (`APIConnectionTimeoutError`)
//! - `is_connect() | is_request() | is_body() | is_decode()` →
//!   `Connection error.` (`APIConnectionError`). `is_decode()` is included
//!   because a truncated mid-stream body surfaces as a decode error
//!   ("unexpected EOF during chunk size line"); a plain JSON-decode failure
//!   would also read as a connection error, which is the safe direction for
//!   the retry classifier.
//! - everything else keeps `reqwest`'s own display text.

use crate::utils::error_body::truncate_error_text;

/// Cap for the composed message (`truncate_error_text` appends the marker).
/// Real source chains are short; this only guards pathological OS strings.
pub const MAX_TRANSPORT_ERROR_CHARS: usize = 1000;

/// Formats a `reqwest` transport failure the way the upstream provider SDKs
/// surface it, so both the retry classifier and users see the cause instead
/// of an opaque `error sending request for url (...)`.
pub fn format_reqwest_error(error: &reqwest::Error) -> String {
    let canonical = if error.is_timeout() {
        "Request timed out."
    } else if error.is_connect() || error.is_request() || error.is_body() || error.is_decode() {
        "Connection error."
    } else {
        return error.to_string();
    };
    let mut message = format!("{canonical} {error}");
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    truncate_error_text(&message, MAX_TRANSPORT_ERROR_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ApiKind, AssistantRole, StopReason, TextContent, Usage};
    use crate::utils::retry::is_retryable_assistant_error;
    use std::net::TcpListener;

    fn assistant_error(error_message: &str) -> crate::types::AssistantMessage {
        crate::types::AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![crate::types::AssistantContent::Text(TextContent {
                text: String::new(),
                text_signature: None,
            })],
            api: ApiKind::from("openai-completions"),
            provider: "mock".to_owned(),
            model: "mock-1".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Error,
            error_message: Some(error_message.to_owned()),
            timestamp: 0,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        }
    }

    /// A closed local port yields a `Connect` error (`is_connect()` true); the
    /// message must lead with the SDK wording and retain the OS cause. The
    /// classifier (`connection.?error`) must accept the result — this is the
    /// integration contract with `utils::retry`.
    #[tokio::test]
    async fn connect_refused_is_connection_error_with_cause() {
        // Bind then drop to reserve a port that is guaranteed closed.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        let error = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .expect_err("closed port must fail");
        assert!(error.is_connect(), "expected connect error: {error:?}");
        let message = format_reqwest_error(&error);
        assert!(
            message.starts_with("Connection error."),
            "message: {message}"
        );
        assert!(
            message.len() > "Connection error. ".len(),
            "cause chain must be retained: {message}"
        );
        assert!(
            is_retryable_assistant_error(&assistant_error(&message)),
            "classifier must retry: {message}"
        );
    }

    /// A server that accepts but never answers produces a timeout; the
    /// message must lead with `Request timed out.` (upstream
    /// `APIConnectionTimeoutError` wording) so the classifier's `timed? out`
    /// pattern matches.
    #[tokio::test]
    async fn request_timeout_is_request_timed_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        // Keep the accepted connection open without responding.
        let hold = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.expect("accept");
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });
        let error = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(50))
            .build()
            .expect("client")
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect_err("silent server must time out");
        hold.abort();
        assert!(error.is_timeout(), "expected timeout error: {error:?}");
        let message = format_reqwest_error(&error);
        assert!(
            message.starts_with("Request timed out."),
            "message: {message}"
        );
        assert!(
            is_retryable_assistant_error(&assistant_error(&message)),
            "classifier must retry: {message}"
        );
    }
}
