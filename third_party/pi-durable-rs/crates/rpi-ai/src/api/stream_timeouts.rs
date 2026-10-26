//! Idle-timeout plumbing for provider streaming requests.
//!
//! Upstream bounds provider requests with undici's `headersTimeout` /
//! `bodyTimeout` (http-dispatcher.ts:81-113): both are **idle** timeouts —
//! `headersTimeout` is the maximum gap between sending the request and
//! receiving response headers, `bodyTimeout` the maximum gap between two
//! body chunks. An actively streaming response can therefore run for any
//! total duration; only a *silent* connection is terminated.
//!
//! The Rust port originally mapped `StreamOptions::timeout_ms` to
//! `reqwest::ClientBuilder::timeout`, which is a **total** deadline covering
//! connect + headers + the entire streamed body. Any inference streaming
//! longer than the configured budget (the then-default 5-minute
//! `httpIdleTimeoutMs`; unlimited by default since rpi#54 / D-103) was
//! killed mid-stream even while actively receiving chunks. This module
//! restores the upstream idle semantics:
//!
//! - **connect phase**: adapters set `ClientBuilder::connect_timeout` to the
//!   same budget (replacing the total `timeout`);
//! - **headers phase**: [`send_with_headers_timeout`] bounds the wait from
//!   request start to response headers (upstream `headersTimeout`; the TS
//!   SDKs' `timeout` option behaves the same way — it is cleared as soon as
//!   `fetch` resolves with headers);
//! - **body phase**: [`wrap`] wraps the response byte stream in
//!   [`IdleTimeoutStream`], which errors when no chunk arrives within the
//!   budget and resets its deadline on every received chunk (upstream
//!   `bodyTimeout`).
//!
//! The Codex SSE transport keeps its own absolute-deadline semantics
//! (`AbortSignal.timeout` upstream, see
//! `openai_codex_responses.rs`); it does not use this module.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::Stream;
use futures::StreamExt;
use tokio::time::Sleep;

/// Maps a `timeout_ms` option to a `Duration`, skipping zero values.
///
/// Zero means "disabled" (the SDK layer maps it to a very large value
/// before it reaches providers; this guard keeps direct callers safe).
pub fn idle_timeout(timeout_ms: Option<u64>) -> Option<Duration> {
    let ms = timeout_ms?;
    if ms == 0 {
        return None;
    }
    Some(Duration::from_millis(ms))
}

/// Outcome of a send attempt bounded by [`send_with_headers_timeout`].
#[derive(Debug, PartialEq, Eq)]
pub enum SendOutcome<T, E> {
    /// Response headers arrived.
    Ok(T),
    /// The transport failed before headers arrived.
    Transport(E),
    /// The headers-wait budget elapsed (message mentions `timed out` so
    /// retry classifiers treat it like any transient transport error).
    HeadersTimeout(String),
}

/// Bounds `future` (a pending `request.send()`) with the headers-wait budget
/// (upstream `headersTimeout` / the TS SDKs' `timeout` option, which is
/// cleared as soon as `fetch` resolves with headers — it never covers the
/// streamed body). A `timeout_ms` of `0` or `None` disables the budget.
pub async fn send_with_headers_timeout<F, T, E>(
    future: F,
    timeout_ms: Option<u64>,
) -> SendOutcome<T, E>
where
    F: Future<Output = Result<T, E>>,
{
    let Some(budget) = idle_timeout(timeout_ms) else {
        return match future.await {
            Ok(value) => SendOutcome::Ok(value),
            Err(error) => SendOutcome::Transport(error),
        };
    };
    match tokio::time::timeout(budget, future).await {
        Ok(Ok(value)) => SendOutcome::Ok(value),
        Ok(Err(error)) => SendOutcome::Transport(error),
        Err(_) => SendOutcome::HeadersTimeout(format!(
            "request timed out after {}ms waiting for response headers",
            timeout_ms.unwrap_or_default()
        )),
    }
}

/// Error surfaced by [`IdleTimeoutStream`].
#[derive(Debug)]
pub enum IdleStreamError<E> {
    /// The underlying transport failed.
    Transport(E),
    /// No chunk arrived within the idle budget (upstream `bodyTimeout`).
    IdleTimeout { idle_ms: u64 },
}

impl IdleStreamError<reqwest::Error> {
    /// The provider-facing message for a body-stream failure. The transport
    /// arm uses the upstream-parity wording via
    /// [`crate::utils::transport_error::format_reqwest_error`] (reqwest's
    /// `Display` drops the source chain, which the retry classifier needs);
    /// the idle arm matches the historical wording, which already carries
    /// `timed out`.
    pub fn message(&self) -> String {
        match self {
            IdleStreamError::Transport(error) => {
                crate::utils::transport_error::format_reqwest_error(error)
            }
            IdleStreamError::IdleTimeout { idle_ms } => {
                format!("stream idle timeout: no data for {idle_ms}ms (connection timed out)")
            }
        }
    }
}

impl<E: std::fmt::Display> std::fmt::Display for IdleStreamError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdleStreamError::Transport(error) => error.fmt(f),
            IdleStreamError::IdleTimeout { idle_ms } => write!(
                f,
                "stream idle timeout: no data for {idle_ms}ms (connection timed out)"
            ),
        }
    }
}

/// Stream adapter enforcing an idle (inter-chunk) timeout.
///
/// Every yielded item resets the deadline; a gap longer than the budget
/// resolves the next `poll_next` with
/// [`IdleStreamError::IdleTimeout`]. `timeout_ms: None` (or `0`) disables
/// enforcement (transparent passthrough).
pub struct IdleTimeoutStream<S> {
    inner: S,
    idle_ms: Option<u64>,
    sleep: Pin<Box<Sleep>>,
}

/// Wraps `stream` with an idle timeout derived from `timeout_ms`.
pub fn wrap<S>(stream: S, timeout_ms: Option<u64>) -> IdleTimeoutStream<S> {
    let idle_ms = timeout_ms.filter(|ms| *ms != 0);
    let deadline = idle_ms.map(tokio::time::Duration::from_millis);
    let sleep = Box::pin(match deadline {
        Some(duration) => tokio::time::sleep(duration),
        None => tokio::time::sleep(tokio::time::Duration::MAX),
    });
    IdleTimeoutStream {
        inner: stream,
        idle_ms,
        sleep,
    }
}

impl<S, T, E> Stream for IdleTimeoutStream<S>
where
    S: Stream<Item = Result<T, E>> + Unpin,
{
    type Item = Result<T, IdleStreamError<E>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.poll_next_unpin(cx) {
            Poll::Ready(Some(item)) => {
                if let Some(idle_ms) = this.idle_ms {
                    let deadline =
                        tokio::time::Instant::now() + tokio::time::Duration::from_millis(idle_ms);
                    this.sleep.as_mut().reset(deadline);
                }
                Poll::Ready(Some(item.map_err(IdleStreamError::Transport)))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                let Some(idle_ms) = this.idle_ms else {
                    return Poll::Pending;
                };
                match this.sleep.as_mut().poll(cx) {
                    Poll::Ready(_) => {
                        Poll::Ready(Some(Err(IdleStreamError::IdleTimeout { idle_ms })))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::time::Duration;

    /// A stream that stays pending until the test signals it.
    fn pending_stream() -> futures::stream::Pending<Result<&'static str, String>> {
        futures::stream::pending()
    }

    #[tokio::test]
    async fn none_timeout_is_transparent_passthrough() {
        let mut stream = wrap(
            futures::stream::iter([Ok::<&str, String>("a"), Err("boom".to_owned())]),
            None,
        );
        assert!(matches!(stream.next().await, Some(Ok("a"))));
        assert!(matches!(
            stream.next().await,
            Some(Err(IdleStreamError::Transport(error))) if error == "boom"
        ));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn zero_timeout_is_disabled() {
        let mut stream = wrap(pending_stream(), Some(0));
        // Would hang forever if polled directly; assert it stays pending
        // without an idle error by racing a short sleep.
        let next = stream.next();
        let outcome = tokio::time::timeout(std::time::Duration::from_millis(50), next).await;
        assert!(outcome.is_err(), "disabled timeout must not fire");
    }

    #[tokio::test]
    async fn silent_stream_times_out_with_idle_error() {
        let mut stream = wrap(pending_stream(), Some(40));
        let started = std::time::Instant::now();
        match stream.next().await {
            Some(Err(IdleStreamError::IdleTimeout { idle_ms })) => {
                assert_eq!(idle_ms, 40);
            }
            other => panic!("expected idle timeout, got {other:?}"),
        }
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(35),
            "error must come from the idle deadline, not immediately"
        );
    }

    #[tokio::test]
    async fn slow_but_active_stream_survives_past_total_duration() {
        // Chunks every 20ms for a total of ~120ms with a 50ms idle budget:
        // the total exceeds the budget, but no inter-chunk gap does — the
        // stream must complete (the exact regression the total-timeout bug
        // caused).
        let items = vec![Ok::<u32, String>(1), Ok(2), Ok(3), Ok(4), Ok(5), Ok(6)];
        let stream = futures::stream::iter(items).then(|item| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                item
            })
        });
        let wrapped = wrap(stream, Some(50));
        let collected: Vec<_> = wrapped.collect().await;
        assert_eq!(collected.len(), 6);
        assert!(collected.iter().all(|item| item.is_ok()));
    }

    #[tokio::test]
    async fn idle_error_display_mentions_timeout_for_retry_classification() {
        let error = IdleStreamError::<String>::IdleTimeout { idle_ms: 1234 };
        let message = error.to_string();
        assert!(message.contains("timeout"), "message: {message}");
        assert!(message.contains("1234"), "message: {message}");
    }

    /// `IdleStreamError<reqwest::Error>::message()` must route transport
    /// failures through the upstream-parity normalizer (the retry classifier
    /// reads this string); reverting to `error.to_string()` fails this test.
    #[tokio::test]
    async fn transport_error_message_uses_upstream_wording() {
        let error = reqwest::Client::new()
            .get("http://127.0.0.1:1/v1/x")
            .send()
            .await
            .expect_err("closed port");
        let wrapped = IdleStreamError::Transport(error);
        let message = wrapped.message();
        assert!(
            message.starts_with("Connection error."),
            "message: {message}"
        );
        let idle = IdleStreamError::<reqwest::Error>::IdleTimeout { idle_ms: 1234 };
        assert_eq!(
            idle.message(),
            "stream idle timeout: no data for 1234ms (connection timed out)"
        );
    }

    #[tokio::test]
    async fn headers_timeout_returns_value_without_budget() {
        assert_eq!(
            send_with_headers_timeout(async { Ok::<u8, String>(7) }, None).await,
            SendOutcome::Ok(7)
        );
        assert_eq!(
            send_with_headers_timeout(async { Ok::<u8, String>(8) }, Some(0)).await,
            SendOutcome::Ok(8)
        );
        assert_eq!(
            send_with_headers_timeout(async { Err::<u8, String>("boom".to_owned()) }, None).await,
            SendOutcome::Transport("boom".to_owned())
        );
    }

    #[tokio::test]
    async fn headers_timeout_elapses_with_retryable_message() {
        let outcome =
            send_with_headers_timeout(std::future::pending::<Result<u8, String>>(), Some(30)).await;
        match outcome {
            SendOutcome::HeadersTimeout(message) => {
                assert!(message.contains("timed out"), "message: {message}");
                assert!(message.contains("30ms"), "message: {message}");
            }
            other => panic!("expected headers timeout, got {other:?}"),
        }
    }

    /// rpi#54 (V15-12 FR-A): `idle_timeout()` maps the disabled default
    /// (`0`) to passthrough (`None`) and explicit budgets verbatim — only
    /// the *default* flipped, the helper semantics are unchanged.
    #[test]
    fn idle_timeout_maps_zero_to_passthrough_and_explicit_verbatim() {
        assert_eq!(idle_timeout(None), None);
        assert_eq!(idle_timeout(Some(0)), None);
        assert_eq!(
            idle_timeout(Some(600_000)),
            Some(Duration::from_millis(600_000))
        );
    }

    /// rpi#54 (V15-12 FR-A'): with the new disabled-by-default setting the
    /// SDK layer maps `0` to the "max int32" budget (`sdk.rs`,
    /// 2_147_483_647 ms — mirrored here; the mapping itself is frozen and
    /// untested-in-rpi-ai), so a mock SSE body that stays silent far past
    /// the old 5-minute default must survive and still deliver the
    /// eventual chunk. Virtual time is compressed via a paused tokio clock
    /// (`start_paused` auto-advances when every task sleeps), so >6 min of
    /// silence costs milliseconds of real time — and would instead fire
    /// `IdleTimeout` promptly if the budget were still the old 300 s
    /// default, making the assertion discriminating.
    #[tokio::test(start_paused = true)]
    async fn default_budget_survives_silence_longer_than_old_default() {
        const EFFECTIVE_DEFAULT_MS: u64 = 2_147_483_647;

        // Silent SSE body: no chunks for > 5 min, then one chunk.
        let silent_then_chunk = futures::stream::once(Box::pin(async {
            tokio::time::sleep(Duration::from_secs(6 * 60)).await;
            Ok::<&'static str, String>("data")
        }));
        let mut wrapped = wrap(silent_then_chunk, Some(EFFECTIVE_DEFAULT_MS));

        let started = std::time::Instant::now();
        assert!(
            matches!(wrapped.next().await, Some(Ok("data"))),
            "silent-but-alive stream must survive on the disabled default"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "virtual-time compression must keep the test fast"
        );
    }

    /// rpi#54 (V15-12 FR-A'): user abort stays immediate under the disabled
    /// default — the cancellation path (`stream_cancel::
    /// next_chunk_or_cancelled`) races the token against the body read and
    /// never waits out the (huge) default idle budget.
    #[tokio::test(start_paused = true)]
    async fn default_budget_abort_wins_immediately_mid_silence() {
        const EFFECTIVE_DEFAULT_MS: u64 = 2_147_483_647;

        let mut stalled = wrap(
            futures::stream::pending::<Result<&'static str, String>>(),
            Some(EFFECTIVE_DEFAULT_MS),
        );
        let token = tokio_util::sync::CancellationToken::new();
        tokio::spawn({
            let token = token.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                token.cancel();
            }
        });

        let started = std::time::Instant::now();
        let outcome =
            crate::api::stream_cancel::next_chunk_or_cancelled(&mut stalled, Some(&token)).await;
        assert!(
            matches!(outcome, crate::api::stream_cancel::StreamNext::Cancelled),
            "abort must win mid-silence, got {outcome:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "abort must not wait for the idle budget"
        );
    }
}
