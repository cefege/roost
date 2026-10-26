//! Port of `packages/ai/src/utils/provider-retry.ts` @ pi 0.82.1 (2efa728).
//!
//! Mirrors the pinned OpenAI/Anthropic SDK retry policy around raw reqwest
//! calls: `x-should-retry` header priority, 408/409/429/5xx retryable set,
//! retry-after precedence chain (`retry-after-ms` millis → `retry-after`
//! seconds → `retry-after` HTTP-date → exponential backoff
//! `min(0.5*2^n, 8)s * (1 - rand*0.25)`), interruptible sleep, and the
//! server-requested delay cap (`maxRetryDelayMs`, default 60s, 0 disables) —
//! exceeding it fails immediately with the seconds in the message.
//!
//! Intentional differences:
//! - The JS version inspects SDK error objects (`error.status` / `error.headers`);
//!   rpi adapters construct [`ProviderErrorInfo`] from reqwest responses.
//! - `Math.random` jitter uses a small internal PRNG (not security-sensitive).

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio_util::sync::CancellationToken;

pub const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;

/// Error info extracted from a failed provider HTTP request (the rpi analogue
/// of the SDK's `ProviderError` shape: `status` + `headers` + `message`).
#[derive(Debug, Clone)]
pub struct ProviderErrorInfo {
    pub status: Option<u16>,
    /// Response headers, names lowercased.
    pub headers: Option<HashMap<String, String>>,
    pub message: String,
}

impl ProviderErrorInfo {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .as_ref()
            .and_then(|headers| headers.get(name))
            .map(String::as_str)
    }
}

/// Outcome of a failed [`retry_provider_request`] call.
#[derive(Debug, Clone)]
pub enum RetryError {
    /// The abort signal fired (`AbortError` upstream: "Request aborted").
    Aborted,
    /// The provider request failed and was not (or no longer) retryable.
    Provider(ProviderErrorInfo),
    /// A non-provider failure, e.g. the server-requested retry delay exceeded
    /// `maxRetryDelayMs` (the message carries the seconds, per upstream).
    Message(String),
}

impl RetryError {
    /// Display message matching the upstream `Error.message`.
    pub fn message(&self) -> String {
        match self {
            RetryError::Aborted => "Request aborted".to_owned(),
            RetryError::Provider(info) => info.message.clone(),
            RetryError::Message(message) => message.clone(),
        }
    }

    pub fn is_aborted(&self) -> bool {
        matches!(self, RetryError::Aborted)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProviderRetryOptions {
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    /// HTTP statuses that fail at once although the default policy would
    /// retry them (provider-retry.ts:7, `0cf65d2bf` for the Decisions 504).
    pub no_retry_statuses: Option<Vec<u16>>,
}

/// Mirrors the pinned OpenAI/Anthropic SDK retry policy; review when the
/// upstream helper changes.
fn is_retryable_provider_error(error: &ProviderErrorInfo) -> bool {
    match error.header("x-should-retry") {
        Some("true") => return true,
        Some("false") => return false,
        _ => {}
    }
    match error.status {
        None => true,
        Some(status) => status == 408 || status == 409 || status == 429 || status >= 500,
    }
}

fn validate_server_retry_delay_ms(
    delay_ms: f64,
    max_retry_delay_ms: Option<u64>,
    provider_error_message: &str,
) -> Result<u64, String> {
    let max_delay_ms = max_retry_delay_ms.unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);
    if max_delay_ms > 0 && delay_ms > max_delay_ms as f64 {
        return Err(format!(
            "Server requested {}s retry delay (max: {}s). {}",
            (delay_ms / 1000.0).ceil() as u64,
            (max_delay_ms as f64 / 1000.0).ceil() as u64,
            provider_error_message
        ));
    }
    Ok(delay_ms.max(0.0) as u64)
}

/// Simple non-security PRNG for backoff jitter (`Math.random` upstream).
fn random_unit() -> f64 {
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut state = STATE.load(Ordering::Relaxed);
    if state == 0 {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e3779b97f4a7c15)
            | 1;
        state = match STATE.compare_exchange(0, seed, Ordering::Relaxed, Ordering::Relaxed) {
            // Won the race: this thread's seed is now the state.
            Ok(_) => seed,
            // Lost the race: use the seed another thread installed.
            Err(current) => current,
        };
    }
    // xorshift64*
    state ^= state >> 12;
    state ^= state << 25;
    state ^= state >> 27;
    STATE.store(state, Ordering::Relaxed);
    (state.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
}

fn retry_delay_ms(
    error: &ProviderErrorInfo,
    retry_index: u32,
    max_retry_delay_ms: Option<u64>,
) -> Result<u64, String> {
    if let Some(retry_after_ms) = error.header("retry-after-ms")
        && let Ok(value) = retry_after_ms.parse::<f64>()
        && value.is_finite()
    {
        return validate_server_retry_delay_ms(value, max_retry_delay_ms, &error.message);
    }

    if let Some(retry_after) = error.header("retry-after") {
        let delay_ms = match retry_after.parse::<f64>() {
            Ok(seconds) => seconds * 1000.0,
            Err(_) => {
                let date_ms = httpdate::parse_http_date(retry_after)
                    .ok()
                    .and_then(|date| date.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as f64)
                    .unwrap_or(f64::NAN);
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as f64)
                    .unwrap_or(0.0);
                date_ms - now_ms
            }
        };
        // #9571 (`2bbfcca43`): an unparseable `Retry-After` must fall
        // through to the exponential backoff instead of `NaN.max(0) = 0`
        // (an immediate retry).
        if delay_ms.is_finite() {
            return validate_server_retry_delay_ms(delay_ms, max_retry_delay_ms, &error.message);
        }
    }

    let exponential_delay = (0.5f64 * 2f64.powi(retry_index as i32)).min(8.0) * 1000.0;
    Ok((exponential_delay * (1.0 - random_unit() * 0.25)) as u64)
}

/// Interruptible sleep; returns `false` when the signal fired first.
async fn abortable_sleep(ms: u64, signal: Option<&CancellationToken>) -> bool {
    match signal {
        None => {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            true
        }
        Some(token) => {
            tokio::select! {
                () = tokio::time::sleep(std::time::Duration::from_millis(ms)) => true,
                () = token.cancelled() => false,
            }
        }
    }
}

/// `retryProviderRequest`: reproduces the OpenAI/Anthropic SDK retry behavior
/// with an interruptible backoff sleep. Invoke the underlying request with
/// SDK-level retries disabled (rpi adapters do not enable reqwest retries).
pub async fn retry_provider_request<T, F, Fut>(
    mut request: F,
    options: ProviderRetryOptions,
    signal: Option<&CancellationToken>,
) -> Result<T, RetryError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderErrorInfo>>,
{
    let max_retries = options.max_retries.unwrap_or(0);
    let mut retries_remaining = max_retries;

    loop {
        match request().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                if signal.map(|s| s.is_cancelled()).unwrap_or(false) {
                    return Err(RetryError::Aborted);
                }
                if retries_remaining == 0 || !is_retryable_provider_error(&error) {
                    return Err(RetryError::Provider(error));
                }
                if error.status.is_some_and(|status| {
                    options
                        .no_retry_statuses
                        .as_ref()
                        .is_some_and(|statuses| statuses.contains(&status))
                }) {
                    return Err(RetryError::Provider(error));
                }

                let retry_index = max_retries - retries_remaining;
                retries_remaining -= 1;
                let delay_ms = match retry_delay_ms(&error, retry_index, options.max_retry_delay_ms)
                {
                    Ok(delay_ms) => delay_ms,
                    Err(message) => return Err(RetryError::Message(message)),
                };
                if !abortable_sleep(delay_ms, signal).await {
                    return Err(RetryError::Aborted);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(status: Option<u16>, headers: &[(&str, &str)], message: &str) -> ProviderErrorInfo {
        ProviderErrorInfo {
            status,
            headers: if headers.is_empty() {
                None
            } else {
                Some(
                    headers
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                )
            },
            message: message.to_owned(),
        }
    }

    #[test]
    fn test_random_unit_is_not_constant_zero() {
        // Regression: a compare_exchange seeding bug pinned the PRNG state to
        // 0, making xorshift64*(0) = 0 and jitter a no-op. The global state is
        // process-wide and may already be seeded by other tests — that is
        // fine; only constant-zero is the failure mode.
        let a = random_unit();
        let b = random_unit();
        assert!((0.0..1.0).contains(&a), "unit range: {a}");
        assert!((0.0..1.0).contains(&b), "unit range: {b}");
        assert!(a != 0.0 || b != 0.0, "random_unit pinned at zero");
        assert_ne!(a, b, "consecutive draws must differ");
    }

    #[test]
    fn test_is_retryable_x_should_retry_header_wins() {
        assert!(is_retryable_provider_error(&error(
            Some(400),
            &[("x-should-retry", "true")],
            "m"
        )));
        assert!(!is_retryable_provider_error(&error(
            Some(429),
            &[("x-should-retry", "false")],
            "m"
        )));
    }

    #[test]
    fn test_is_retryable_status_set() {
        assert!(is_retryable_provider_error(&error(None, &[], "m")));
        for status in [408u16, 409, 429, 500, 503] {
            assert!(
                is_retryable_provider_error(&error(Some(status), &[], "m")),
                "{status}"
            );
        }
        for status in [400u16, 401, 403, 404, 422] {
            assert!(
                !is_retryable_provider_error(&error(Some(status), &[], "m")),
                "{status}"
            );
        }
    }

    #[test]
    fn test_retry_after_ms_priority() {
        let err = error(
            Some(429),
            &[("retry-after-ms", "250"), ("retry-after", "5")],
            "m",
        );
        assert_eq!(retry_delay_ms(&err, 0, None).expect("delay"), 250);
    }

    #[test]
    fn test_retry_after_seconds() {
        let err = error(Some(429), &[("retry-after", "2")], "m");
        assert_eq!(retry_delay_ms(&err, 0, None).expect("delay"), 2000);
    }

    #[test]
    fn test_retry_after_http_date() {
        let date = httpdate::fmt_http_date(
            std::time::SystemTime::now() + std::time::Duration::from_secs(3),
        );
        let err = error(Some(429), &[("retry-after", &date)], "m");
        let delay = retry_delay_ms(&err, 0, None).expect("delay");
        assert!(
            (2000..=3100).contains(&delay),
            "delay {delay} should be ~3000ms"
        );
    }

    /// #9571 (`2bbfcca43`): an unparseable `Retry-After` falls back to the
    /// exponential backoff (500ms at index 0), not an immediate retry.
    #[test]
    fn test_unparseable_retry_after_falls_back_to_exponential_backoff() {
        let err = error(Some(429), &[("retry-after", "not-a-date")], "m");
        let delay = retry_delay_ms(&err, 0, None).expect("delay");
        assert!((375..=500).contains(&delay), "exponential delay: {delay}");
        // A non-finite numeric value falls through as well.
        let err = error(Some(429), &[("retry-after", "NaN")], "m");
        let delay = retry_delay_ms(&err, 0, None).expect("delay");
        assert!((375..=500).contains(&delay), "exponential delay: {delay}");
    }

    #[test]
    fn test_server_delay_cap_exceeded_fails_immediately() {
        let err = error(Some(429), &[("retry-after", "90")], "rate limited");
        let result = retry_delay_ms(&err, 0, None);
        assert_eq!(
            result.unwrap_err(),
            "Server requested 90s retry delay (max: 60s). rate limited"
        );
        // 0 disables the cap.
        assert_eq!(retry_delay_ms(&err, 0, Some(0)).expect("delay"), 90_000);
    }

    #[test]
    fn test_exponential_backoff_bounds() {
        let err = error(Some(500), &[], "m");
        for (index, base) in [
            (0u32, 500.0f64),
            (1, 1000.0),
            (2, 2000.0),
            (3, 4000.0),
            (4, 8000.0),
            (5, 8000.0),
        ] {
            let delay = retry_delay_ms(&err, index, None).expect("delay") as f64;
            assert!(
                delay >= base * 0.749 && delay <= base,
                "index {index}: {delay} not in [{}, {base}]",
                base * 0.75
            );
        }
    }

    #[tokio::test]
    async fn test_retry_provider_request_succeeds_after_retry() {
        let calls = std::sync::Arc::new(AtomicU64::new(0));
        let calls2 = calls.clone();
        let result = retry_provider_request(
            move || {
                let calls = calls2.clone();
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(error(Some(500), &[], "boom"))
                    } else {
                        Ok(42)
                    }
                }
            },
            ProviderRetryOptions {
                max_retries: Some(1),
                max_retry_delay_ms: None,
                ..Default::default()
            },
            None,
        )
        .await;
        assert_eq!(result.expect("ok"), 42);
    }

    #[tokio::test]
    async fn test_retry_provider_request_non_retryable_status() {
        let result: Result<(), RetryError> = retry_provider_request(
            || async { Err(error(Some(401), &[], "unauthorized")) },
            ProviderRetryOptions {
                max_retries: Some(3),
                max_retry_delay_ms: None,
                ..Default::default()
            },
            None,
        )
        .await;
        match result {
            Err(RetryError::Provider(info)) => assert_eq!(info.status, Some(401)),
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    /// `noRetryStatuses` (provider-retry.ts:7 @ 0cf65d2bf): a normally
    /// retryable status fails at once when listed (the Decisions 504).
    #[tokio::test]
    async fn test_retry_provider_request_no_retry_statuses_fail_fast() {
        let calls = std::sync::Arc::new(AtomicU64::new(0));
        let calls2 = calls.clone();
        let result: Result<(), RetryError> = retry_provider_request(
            move || {
                let calls = calls2.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(error(Some(504), &[], "gateway timeout"))
                }
            },
            ProviderRetryOptions {
                max_retries: Some(3),
                max_retry_delay_ms: None,
                no_retry_statuses: Some(vec![504]),
            },
            None,
        )
        .await;
        match result {
            Err(RetryError::Provider(info)) => assert_eq!(info.status, Some(504)),
            other => panic!("expected provider error, got {other:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "504 must not be retried");

        // Without the opt-out the same status retries.
        let calls = std::sync::Arc::new(AtomicU64::new(0));
        let calls2 = calls.clone();
        let result: Result<(), RetryError> = retry_provider_request(
            move || {
                let calls = calls2.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(error(Some(504), &[], "gateway timeout"))
                }
            },
            ProviderRetryOptions {
                max_retries: Some(1),
                max_retry_delay_ms: None,
                ..Default::default()
            },
            None,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "default policy retries 504"
        );
    }

    #[tokio::test]
    async fn test_retry_provider_request_delay_cap_is_terminal() {
        let result: Result<(), RetryError> = retry_provider_request(
            || async { Err(error(Some(429), &[("retry-after", "90")], "slow down")) },
            ProviderRetryOptions {
                max_retries: Some(3),
                max_retry_delay_ms: None,
                ..Default::default()
            },
            None,
        )
        .await;
        match result {
            Err(RetryError::Message(message)) => {
                assert_eq!(
                    message,
                    "Server requested 90s retry delay (max: 60s). slow down"
                );
            }
            other => panic!("expected delay-cap message, got {other:?}"),
        }
    }
}
