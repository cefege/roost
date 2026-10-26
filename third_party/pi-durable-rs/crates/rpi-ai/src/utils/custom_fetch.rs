//! Per-request custom fetch channel (R2.7.4; upstream `027a58479` @ 4181f66,
//! `StreamOptions.fetch` / `ImagesOptions.fetch` in `packages/ai/src/types.ts`).
//!
//! Upstream injects `fetch` into the SDK clients; rpi adapters drive reqwest
//! directly, so the channel is plumbed here instead: when a custom [`FetchFn`]
//! is set, the built reqwest request is translated into the neutral
//! [`FetchRequest`] wire shape, the custom fetch is invoked, and its streaming
//! [`FetchResponse`] is wrapped back into a `reqwest::Response` so downstream
//! SSE parsing, error normalization, and `onResponse` reporting stay identical
//! across both paths. When no custom fetch is set, the reqwest default path is
//! byte-for-byte the previous behavior.
//!
//! Intentional differences:
//! - `timeout_ms` is a reqwest client-level knob; it does not apply to a
//!   custom fetch (the injected implementation owns its own timing, like an
//!   upstream `fetch` wrapper would).
//! - Non-UTF-8 request header values cannot be represented in the neutral
//!   [`FetchRequest`] shape and are dropped on the custom path (adapter
//!   headers are ASCII in practice).

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::types::{FetchFn, FetchRequest, FetchResponse};
use crate::utils::provider_retry::ProviderErrorInfo;
use crate::utils::transport_error::format_reqwest_error;

/// Outcome of a failed [`send_provider_request`] attempt. The variants keep
/// the reqwest error intact so adapters with special reqwest mappings (e.g.
/// Mistral's timeout message) keep working unchanged on the default path.
#[derive(Debug)]
pub enum SendFailure {
    /// The abort signal fired before the response arrived.
    Aborted,
    /// The headers-wait budget elapsed before response headers arrived
    /// (upstream undici `headersTimeout`; see `api::stream_timeouts`).
    HeadersTimedOut(String),
    /// The reqwest transport failed (default path).
    Reqwest(reqwest::Error),
    /// The custom fetch failed, or its response could not be bridged.
    Custom(ProviderErrorInfo),
}

impl SendFailure {
    /// The standard adapter mapping: abort and reqwest failures become the
    /// same [`ProviderErrorInfo`] the inline `request.send()` + abort-select
    /// blocks produced before the fetch channel existed.
    pub fn into_provider_error_info(self) -> ProviderErrorInfo {
        match self {
            SendFailure::Aborted => ProviderErrorInfo {
                status: None,
                headers: None,
                message: "Request was aborted".to_owned(),
            },
            SendFailure::HeadersTimedOut(message) => ProviderErrorInfo {
                status: None,
                headers: None,
                message,
            },
            SendFailure::Reqwest(error) => ProviderErrorInfo {
                status: error.status().map(|status| status.as_u16()),
                headers: None,
                message: format_reqwest_error(&error),
            },
            SendFailure::Custom(info) => info,
        }
    }

    /// Display message matching the pre-channel `error.to_string()` mapping
    /// (pi_messages / codex style), with the upstream-parity transport
    /// wording for reqwest failures (see `utils::transport_error`).
    pub fn message(&self) -> String {
        match self {
            SendFailure::Aborted => "Request was aborted".to_owned(),
            SendFailure::HeadersTimedOut(message) => message.clone(),
            SendFailure::Reqwest(error) => format_reqwest_error(error),
            SendFailure::Custom(info) => info.message.clone(),
        }
    }
}

/// Sends one attempt of a provider request, honoring the per-request custom
/// fetch channel (R2.7.4). With `fetch: None` this is exactly the previous
/// inline behavior: `request.send()` raced against the abort signal — plus
/// the headers-wait budget: when `timeout_ms` is set, the wait from request
/// start to response headers is bounded (upstream undici `headersTimeout`;
/// the TS SDKs' `timeout` option is likewise cleared once headers arrive, so
/// it never covers the streamed body). A `timeout_ms` of `0` disables the
/// budget.
pub async fn send_provider_request(
    request: reqwest::RequestBuilder,
    fetch: Option<&FetchFn>,
    signal: Option<&CancellationToken>,
    timeout_ms: Option<u64>,
) -> Result<reqwest::Response, SendFailure> {
    match fetch {
        None => {
            let send = async {
                match crate::api::stream_timeouts::send_with_headers_timeout(
                    request.send(),
                    timeout_ms,
                )
                .await
                {
                    crate::api::stream_timeouts::SendOutcome::Ok(response) => Ok(response),
                    crate::api::stream_timeouts::SendOutcome::Transport(error) => {
                        Err(SendFailure::Reqwest(error))
                    }
                    crate::api::stream_timeouts::SendOutcome::HeadersTimeout(message) => {
                        Err(SendFailure::HeadersTimedOut(message))
                    }
                }
            };
            match signal {
                Some(token) => tokio::select! {
                    outcome = send => outcome,
                    () = token.cancelled() => Err(SendFailure::Aborted),
                },
                None => send.await,
            }
        }
        Some(fetch) => {
            let built = request.build().map_err(|error| {
                SendFailure::Custom(ProviderErrorInfo {
                    status: None,
                    headers: None,
                    message: error.to_string(),
                })
            })?;
            let fetch_request = fetch_request_from(&built).map_err(|message| {
                SendFailure::Custom(ProviderErrorInfo {
                    status: None,
                    headers: None,
                    message,
                })
            })?;
            let call = fetch(fetch_request);
            let response = match signal {
                Some(token) => tokio::select! {
                    outcome = call => outcome,
                    () = token.cancelled() => return Err(SendFailure::Aborted),
                },
                None => call.await,
            };
            let response = response.map_err(|error| {
                SendFailure::Custom(ProviderErrorInfo {
                    status: None,
                    headers: None,
                    message: error.to_string(),
                })
            })?;
            into_reqwest_response(response).map_err(|message| {
                SendFailure::Custom(ProviderErrorInfo {
                    status: None,
                    headers: None,
                    message,
                })
            })
        }
    }
}

/// Translates the built reqwest request into the neutral [`FetchRequest`]
/// wire shape. All adapters send reusable bodies (`.json()` / `.body(Vec)`),
/// so body extraction always succeeds; a non-reusable body is an internal
/// error, not silently dropped.
fn fetch_request_from(request: &reqwest::Request) -> Result<FetchRequest, String> {
    let headers = request
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect();
    let body = match request.body() {
        None => None,
        Some(body) => Some(
            body.as_bytes()
                .ok_or_else(|| "custom fetch requires a reusable request body".to_owned())?
                .to_vec(),
        ),
    };
    Ok(FetchRequest {
        method: request.method().as_str().to_owned(),
        url: request.url().to_string(),
        headers,
        body,
    })
}

/// Bridges a custom [`FetchResponse`] back into a `reqwest::Response`
/// (`impl From<http::Response<Body>>`, reqwest 0.12) so the downstream
/// streaming/error code is shared with the default path.
fn into_reqwest_response(response: FetchResponse) -> Result<reqwest::Response, String> {
    let mut builder = http::Response::builder().status(response.status);
    for (name, value) in &response.headers {
        builder = builder.header(name, value);
    }
    let body = reqwest::Body::wrap_stream(response.body.map(|chunk| chunk.map(bytes::Bytes::from)));
    builder
        .body(body)
        .map(reqwest::Response::from)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn custom_fetch(
        status: u16,
        body: &'static [u8],
        captured: Arc<Mutex<Option<FetchRequest>>>,
    ) -> FetchFn {
        Arc::new(move |request: FetchRequest| {
            *captured.lock().unwrap_or_else(|e| e.into_inner()) = Some(request);
            Box::pin(async move {
                Ok(FetchResponse {
                    status,
                    headers: vec![("content-type".to_owned(), "application/json".to_owned())],
                    body: Box::pin(futures::stream::once(async move { Ok(body.to_vec()) })),
                })
            })
        })
    }

    #[tokio::test]
    async fn test_default_path_unchanged_without_fetch() {
        let client = reqwest::Client::new();
        let request = client
            .post("http://127.0.0.1:1/v1/x")
            .json(&serde_json::json!({}));
        let result = send_provider_request(request, None, None, None).await;
        match result {
            Err(failure @ SendFailure::Reqwest(_)) => {
                if let SendFailure::Reqwest(error) = &failure {
                    assert!(error.is_connect());
                }
                // Wiring guard: reverting the formatter call in
                // `into_provider_error_info`/`message` fails here.
                assert!(
                    failure.message().starts_with("Connection error."),
                    "message: {}",
                    failure.message()
                );
                assert!(
                    failure
                        .into_provider_error_info()
                        .message
                        .starts_with("Connection error.")
                );
            }
            other => panic!("expected reqwest connect error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_headers_timeout_elapses_before_response() {
        // A server that accepts the connection but never answers: the wait
        // for response headers must be bounded by the headers budget and
        // surface HeadersTimedOut with a retry-classifiable message.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            // Accept and hold the socket open without responding.
            while let Ok((socket, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let mut socket = socket;
                // Drain the request so the client write side completes.
                use tokio::io::AsyncReadExt;
                let _ = socket.read(&mut buf).await;
                std::future::pending::<()>().await;
            }
        });
        let client = reqwest::Client::new();
        let request = client
            .post(format!("http://{addr}/v1/x"))
            .json(&serde_json::json!({}));
        let result = send_provider_request(request, None, None, Some(50)).await;
        match result {
            Err(SendFailure::HeadersTimedOut(message)) => {
                assert!(message.contains("timed out"), "message: {message}");
                let info = SendFailure::HeadersTimedOut(message).into_provider_error_info();
                assert_eq!(info.status, None, "transport errors are retryable");
            }
            other => panic!("expected headers timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_custom_fetch_receives_translated_request() {
        let captured = Arc::new(Mutex::new(None));
        let fetch = custom_fetch(401, b"{}", captured.clone());
        let client = reqwest::Client::new();
        let request = client
            .post("http://upstream.test/v1/chat?api-version=1")
            .header("authorization", "Bearer k")
            .json(&serde_json::json!({"a": 1}));
        let response = send_provider_request(request, Some(&fetch), None, None)
            .await
            .expect("custom fetch response");
        assert_eq!(response.status().as_u16(), 401);
        let text = response.text().await.expect("body text");
        assert_eq!(text, "{}");

        let request = captured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("request captured");
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "http://upstream.test/v1/chat?api-version=1");
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name == "authorization" && value == "Bearer k")
        );
        let body: serde_json::Value =
            serde_json::from_slice(&request.body.expect("json body")).expect("body json");
        assert_eq!(body, serde_json::json!({"a": 1}));
    }

    #[tokio::test]
    async fn test_custom_fetch_abort_maps_to_aborted() {
        let fetch: FetchFn = Arc::new(|_request| {
            Box::pin(async move {
                futures::future::pending::<()>().await;
                unreachable!("abort wins the race")
            })
        });
        let token = CancellationToken::new();
        token.cancel();
        let client = reqwest::Client::new();
        let request = client.post("http://upstream.test/v1/x");
        let result = send_provider_request(request, Some(&fetch), Some(&token), None).await;
        assert!(matches!(result, Err(SendFailure::Aborted)));
        let info = match result {
            Err(failure) => failure.into_provider_error_info(),
            Ok(_) => panic!("expected failure"),
        };
        assert_eq!(info.message, "Request was aborted");
        assert_eq!(info.status, None);
    }
}
