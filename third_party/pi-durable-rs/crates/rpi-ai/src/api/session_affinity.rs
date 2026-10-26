//! Session-affinity header seam (#9102 / #9326 / #9629, upstream cluster
//! B FR-C).
//!
//! Single request-assembly point for per-request derived session-affinity
//! headers (design baseline §2.1: the three affinity providers share one
//! "per-request derived header" injection point instead of three scattered
//! edits):
//! - OpenRouter (#9102, bbb61e34a): `x-session-id` by default on Chat
//!   Completions (`openai_completions::detect_compat`) and Anthropic
//!   Messages ([`anthropic_session_affinity_headers`]), opt-out via
//!   `compat.sendSessionAffinityHeaders: false`.
//! - OpenCode / OpenCode Go (#9326, 561a2e066): `x-opencode-session` on every
//!   adapter via the [`WithSessionHeader`] provider decorator (port of
//!   upstream `providers/opencode-headers.ts`).
//! - Baseten (#9629, 6671c6047): catalog `compat.sendSessionAffinityHeaders`
//!   flows through the existing Chat Completions injection (`x-session-affinity`
//!   et al.); the catalog data lands with the V15-03 regen.

use std::sync::Arc;

use crate::models::ProviderStreams;
use crate::types::{
    Model, ProviderHeaders, SessionAffinityFormat, SimpleStreamOptions, StreamOptions,
    TranscriptContext,
};
use crate::utils::event_stream::AssistantMessageEventStream;

/// `isOpenRouter` (anthropic-messages.ts getAnthropicCompat; the same
/// detection already lives inline in `openai_completions::detect_compat` and
/// `openai_responses::detect_session_affinity_format`).
pub fn is_openrouter(provider: &str, base_url: &str) -> bool {
    provider == "openrouter" || base_url.contains("openrouter.ai")
}

/// Anthropic Messages affinity headers (anthropic-messages.ts createClient):
/// `x-session-id` for the OpenRouter format, `x-session-affinity` otherwise.
pub fn anthropic_session_affinity_headers(
    send: bool,
    format: Option<SessionAffinityFormat>,
    session_id: Option<&str>,
) -> Option<ProviderHeaders> {
    let session_id = session_id.filter(|_| send)?;
    let header = if format == Some(SessionAffinityFormat::Openrouter) {
        "x-session-id"
    } else {
        "x-session-affinity"
    };
    Some(
        [(header.to_owned(), Some(session_id.to_owned()))]
            .into_iter()
            .collect(),
    )
}

/// `hasHeader` (opencode-headers.ts): case-insensitive key probe.
fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    headers.is_some_and(|headers| headers.keys().any(|key| key.eq_ignore_ascii_case(name)))
}

/// `withSessionHeader`: adds the provider session header to caller options
/// when a session id is set and the caller did not set the header explicitly
/// (explicit caller headers win — #9326 "Preserve explicit caller header
/// overrides").
fn inject_session_header(options: &mut StreamOptions, header_name: &str) {
    let Some(session_id) = options.session_id.clone() else {
        return;
    };
    if has_header(options.request.headers.as_ref(), header_name) {
        return;
    }
    options
        .request
        .headers
        .get_or_insert_with(ProviderHeaders::default)
        .insert(header_name.to_owned(), Some(session_id));
}

/// Port of upstream `withOpenCodeSessionHeader` (providers/opencode-headers.ts,
/// #9326): wraps a [`ProviderStreams`] so both `stream` and `streamSimple`
/// dispatch with the derived session header pre-merged into caller options —
/// ahead of the adapter's own header assembly, so the existing merge
/// precedence (model/options override) is preserved.
pub struct WithSessionHeader {
    inner: Arc<dyn ProviderStreams>,
    header_name: &'static str,
}

impl WithSessionHeader {
    pub fn new(inner: Arc<dyn ProviderStreams>, header_name: &'static str) -> Self {
        Self { inner, header_name }
    }
}

impl ProviderStreams for WithSessionHeader {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        let options = options.map(|mut options| {
            inject_session_header(&mut options, self.header_name);
            options
        });
        self.inner.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String> {
        let options = options.map(|mut options| {
            inject_session_header(&mut options.stream, self.header_name);
            options
        });
        self.inner.stream_simple(model, context, options)
    }
}

/// OpenCode's required per-conversation routing header (#9326).
pub const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";

/// `withOpenCodeSessionHeader` (opencode-headers.ts): decorator factory used
/// by the `opencode` / `opencode-go` provider api maps.
pub fn with_opencode_session_header(inner: Arc<dyn ProviderStreams>) -> Arc<dyn ProviderStreams> {
    Arc::new(WithSessionHeader::new(inner, OPENCODE_SESSION_HEADER))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn stream_options(session_id: Option<&str>, headers: Option<ProviderHeaders>) -> StreamOptions {
        StreamOptions {
            request: crate::types::ProviderRequestOptions {
                headers,
                ..Default::default()
            },
            session_id: session_id.map(str::to_owned),
            ..Default::default()
        }
    }

    /// Minimal loopback capture server for wiring tests: accepts one
    /// connection, records the raw request bytes (method line + headers),
    /// answers 500 once the header block arrived, and closes (the adapters'
    /// error paths end the stream — only the captured request matters here).
    pub(crate) async fn capture_server() -> (String, tokio::sync::oneshot::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            loop {
                let mut chunk = [0u8; 1024];
                let read = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    socket.read(&mut chunk),
                )
                .await;
                let n = read.map(|n| n.unwrap_or(0)).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\n\r\n",
                        )
                        .await;
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
        });
        (format!("http://{addr}"), rx)
    }

    /// Whether the captured raw request carries the header.
    pub(crate) fn captured_has_header(raw: &str, name: &str, value: &str) -> bool {
        raw.lines().any(|line| {
            let line = line.trim_end_matches('\r');
            let Some((key, val)) = line.split_once(':') else {
                return false;
            };
            key.trim().eq_ignore_ascii_case(name) && val.trim() == value
        })
    }

    #[test]
    fn injects_header_only_with_session_and_respects_overrides() {
        // No session id: unchanged (no header map created).
        let mut options = stream_options(None, None);
        inject_session_header(&mut options, OPENCODE_SESSION_HEADER);
        assert!(options.request.headers.is_none());

        // Session id set: header derived.
        let mut options = stream_options(Some("sess-1"), None);
        inject_session_header(&mut options, OPENCODE_SESSION_HEADER);
        assert_eq!(
            options.request.headers.expect("headers")["x-opencode-session"],
            Some("sess-1".to_owned())
        );

        // Explicit caller header wins, regardless of case (#9326).
        let explicit: ProviderHeaders =
            [("X-OpenCode-Session".to_owned(), Some("custom".to_owned()))].into();
        let mut options = stream_options(Some("sess-1"), Some(explicit));
        inject_session_header(&mut options, OPENCODE_SESSION_HEADER);
        let headers = options.request.headers.expect("headers");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers["X-OpenCode-Session"], Some("custom".to_owned()));
    }

    #[test]
    fn anthropic_affinity_header_selection() {
        // OpenRouter format → x-session-id only (#9102).
        let headers = anthropic_session_affinity_headers(
            true,
            Some(SessionAffinityFormat::Openrouter),
            Some("sess-1"),
        )
        .expect("headers");
        assert_eq!(headers["x-session-id"], Some("sess-1".to_owned()));
        assert!(!headers.contains_key("x-session-affinity"));

        // Unset format (e.g. Fireworks catalog entries) → x-session-affinity.
        let headers =
            anthropic_session_affinity_headers(true, None, Some("sess-1")).expect("headers");
        assert_eq!(headers["x-session-affinity"], Some("sess-1".to_owned()));

        // Disabled or no session → none.
        assert!(anthropic_session_affinity_headers(false, None, Some("s")).is_none());
        assert!(anthropic_session_affinity_headers(true, None, None).is_none());
    }

    #[test]
    fn openrouter_detection() {
        assert!(is_openrouter("openrouter", "https://example.com"));
        assert!(is_openrouter("custom", "https://openrouter.ai/api/v1"));
        assert!(!is_openrouter("openai", "https://api.openai.com/v1"));
    }

    /// #9326 e2e: the decorator threads `x-opencode-session` through a real
    /// adapter (Anthropic Messages) onto the wire — wiring-level proof for the
    /// opencode / opencode-go api maps.
    #[tokio::test]
    async fn opencode_session_header_reaches_the_wire() {
        let (base_url, raw_rx) = capture_server().await;
        let model: Model = serde_json::from_value(serde_json::json!({
            "id": "claude-sonnet-4.6", "name": "m", "api": "anthropic-messages",
            "provider": "opencode", "baseUrl": base_url, "reasoning": false, "input": ["text"],
            "cost": {"input": 1.0, "output": 1.0, "cacheRead": 0.1, "cacheWrite": 1.0},
            "contextWindow": 1000, "maxTokens": 100
        }))
        .expect("model");
        let wrapped = with_opencode_session_header(std::sync::Arc::new(
            crate::api::anthropic_messages::AnthropicMessages,
        ) as Arc<dyn ProviderStreams>);
        let context = crate::types::Context::default();
        let options = stream_options(Some("opencode-sess-1"), None);
        let mut options = options;
        options.request.api_key = Some("test-key".to_owned());
        let stream = wrapped.stream(
            &model,
            &crate::utils::transcript::normalize_context(&context),
            Some(options),
        );
        let _ = stream.result().await;
        let raw = tokio::time::timeout(std::time::Duration::from_secs(5), raw_rx)
            .await
            .expect("captured")
            .expect("server sent");
        assert!(
            captured_has_header(&raw, "x-opencode-session", "opencode-sess-1"),
            "x-opencode-session must be sent; raw request:\n{raw}"
        );
    }
}
