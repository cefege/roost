//! Port of `packages/ai/src/utils/retry.ts` @ pi 0.84.1+ (4181f66).
//!
//! Outer assistant-call retry: error classification regex tables + bounded
//! exponential backoff (`baseDelayMs * 2^(attempt-1)`).

use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;

use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::types::{AssistantMessage, StopReason};

fn build_provider_error_pattern(patterns: &[&str]) -> Regex {
    // invariant: pinned literal patterns ported from retry.ts; they compile
    // (verified by the tests below).
    Regex::new(&format!("(?i){}", patterns.join("|"))).expect("static retry pattern must compile")
}

fn non_retryable_provider_limit_error_pattern() -> &'static Regex {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        build_provider_error_pattern(&[
            // OpenCode Go/free-tier subscription/account limits.
            "GoUsageLimitError",
            "FreeUsageLimitError",
            "Monthly usage limit reached",
            "available balance",
            // Generic quota/budget/billing exhaustion.
            "insufficient_quota",
            "out of budget",
            "quota exceeded",
            "billing",
            // Sign in with ChatGPT: the subscription's shared usage limit,
            // which resets after hours rather than seconds (`02eed88fd`).
            "subscription_sharing_usage_limit_exceeded",
        ])
    });
    &PATTERN
}

fn retryable_provider_error_pattern() -> &'static Regex {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        build_provider_error_pattern(&[
            "overloaded",
            // #10543 (8b5708dbb): transient server-busy errors.
            "server_busy",
            "servers are currently busy",
            // #9669 (e98f287ee): Azure peak-load capacity errors are
            // transient — retry instead of surfacing a hard failure.
            "currently experiencing high demand",
            // #10278 (3874b3e98): transient provider capacity errors.
            "model is at capacity",
            "rate.?limit",
            "too many requests",
            "429",
            "500",
            "502",
            "503",
            "504",
            // #9627 (e5d18382a): Cloudflare-origin 520s are retryable,
            // like the 52x/54x statuses already listed.
            "520",
            "524",
            "service.?unavailable",
            "server.?error",
            "internal.?error",
            // Wrapper/provider text for transient upstream failures, including
            // OpenRouter "Provider returned error" responses (#2264).
            "provider.?returned.?error",
            "exceeded request buffer limit while retrying upstream",
            "network.?error",
            "connection.?error",
            "connection.?refused",
            "connection.?lost",
            "other side closed",
            "fetch failed",
            "getaddrinfo",
            "ENOTFOUND",
            "EAI_AGAIN",
            "upstream.?connect",
            "reset before headers",
            "socket hang up",
            "socket connection was closed",
            "timed? out",
            "timeout",
            "terminated",
            "websocket.?closed",
            "websocket.?error",
            "ended without",
            "stream ended before message_stop",
            "stream ended before a terminal response event",
            "http2 request did not get a response",
            // #10379 (5b6c792b4): Node ERR_HTTP2_STREAM_CANCEL — the HTTP/2
            // session died before the request was sent, e.g. after the
            // Bedrock SDK's 5-minute session timeout.
            "pending stream has been canceled",
            "retry delay",
            "you can retry your request",
            "try your request again",
            "please retry your request",
            "ResourceExhausted",
            // Sign in with ChatGPT: usage or user data temporarily
            // unavailable (`02eed88fd`).
            "subscription_sharing_usage_unavailable",
            "subscription_sharing_user_unavailable",
        ])
    });
    &PATTERN
}

/// `RetryPolicy` — matches `settings.retry` in coding-agent.
///
/// #8826 (`c37b0e03b` "cap agent retry backoff"): [`max_agent_delay_ms`]
/// caps each computed delay; `None` defaults to
/// [`DEFAULT_MAX_AGENT_RETRY_DELAY_MS`] (60s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub enabled: bool,
    /// Max retry attempts (0 = no retries). The initial call never counts.
    pub max_retries: u32,
    /// Base delay in ms. Per-attempt delay is `baseDelayMs * 2^(attempt-1)`.
    pub base_delay_ms: u64,
    /// Optional cap for agent-level retry delays in ms (`maxAgentDelayMs`).
    /// `None` → [`DEFAULT_MAX_AGENT_RETRY_DELAY_MS`].
    pub max_agent_delay_ms: Option<u64>,
}

/// `DEFAULT_MAX_AGENT_RETRY_DELAY_MS` (retry.ts @ c37b0e03b, #8826).
pub const DEFAULT_MAX_AGENT_RETRY_DELAY_MS: u64 = 60_000;

/// `retryDelayMs` (retry.ts @ c37b0e03b, #8826): exponential backoff with a
/// cap — `min(baseDelayMs * 2^(attempt-1), maxAgentDelayMs ?? 60s)`.
/// `u64` arithmetic saturates, so upstream's `Number.isSafeInteger`
/// guard is covered by `saturating_mul`/`saturating_pow`.
pub fn retry_delay_ms(base_delay_ms: u64, max_agent_delay_ms: Option<u64>, attempt: u32) -> u64 {
    let delay = base_delay_ms.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1)));
    delay.min(max_agent_delay_ms.unwrap_or(DEFAULT_MAX_AGENT_RETRY_DELAY_MS))
}

type RetryCallback<Args> = dyn Fn(Args) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync;

/// `onRetryScheduled` args: (attempt, max_attempts, delay_ms, error_message).
pub type RetryScheduledArgs = (u32, u32, u64, String);
/// `onRetryFinished` args: (success, attempt, final_error).
pub type RetryFinishedArgs = (bool, u32, Option<String>);

/// Optional callbacks emitted by [`retry_assistant_call`] around each retry.
#[derive(Default)]
pub struct RetryCallbacks {
    /// Before the backoff sleep of each attempt: (attempt, max_attempts,
    /// delay_ms, error_message).
    pub on_retry_scheduled: Option<Box<RetryCallback<RetryScheduledArgs>>>,
    /// After the backoff sleep, immediately before the retried call starts.
    pub on_retry_attempt_start: Option<Box<RetryCallback<()>>>,
    /// Once when the loop ends: (success, attempt, final_error).
    pub on_retry_finished: Option<Box<RetryCallback<RetryFinishedArgs>>>,
}

/// `retryAssistantCall`: runs a single assistant-producing call with bounded
/// retry on transient errors. Aborts are terminal and never retried; aborts
/// during the backoff sleep are normalized to an aborted `AssistantMessage`.
pub async fn retry_assistant_call<F, Fut>(
    mut produce: F,
    policy: Option<&RetryPolicy>,
    signal: Option<&CancellationToken>,
    callbacks: Option<&RetryCallbacks>,
) -> AssistantMessage
where
    F: FnMut() -> Fut,
    Fut: Future<Output = AssistantMessage>,
{
    let max_attempts = match policy {
        Some(policy) if policy.enabled => policy.max_retries,
        _ => 0,
    };

    let mut attempt: u32 = 0;
    let mut last_retry: Option<(u32, String)> = None;
    loop {
        let response = produce().await;

        // Abort: terminal but not successful. Never retry an aborted message.
        if response.stop_reason == StopReason::Aborted {
            if let Some((attempt, _)) = last_retry
                && let Some(cb) = callbacks.and_then(|c| c.on_retry_finished.as_ref())
            {
                cb((false, attempt, None)).await;
            }
            return response;
        }

        // Success: non-error, non-abort responses return as-is.
        if response.stop_reason != StopReason::Error {
            if let Some((attempt, _)) = last_retry
                && let Some(cb) = callbacks.and_then(|c| c.on_retry_finished.as_ref())
            {
                cb((true, attempt, None)).await;
            }
            return response;
        }

        // Non-retryable, or budget exhausted: return the final error message.
        if attempt >= max_attempts || !is_retryable_assistant_error(&response) {
            if let Some((attempt, _)) = last_retry
                && let Some(cb) = callbacks.and_then(|c| c.on_retry_finished.as_ref())
            {
                cb((false, attempt, response.error_message.clone())).await;
            }
            return response;
        }

        attempt += 1;
        let error_message = response
            .error_message
            .clone()
            .unwrap_or_else(|| "Unknown error".to_owned());
        last_retry = Some((attempt, error_message.clone()));
        let policy_base = policy.map(|p| p.base_delay_ms).unwrap_or(0);
        let policy_cap = policy.and_then(|p| p.max_agent_delay_ms);
        let delay_ms = retry_delay_ms(policy_base, policy_cap, attempt);
        if let Some(cb) = callbacks.and_then(|c| c.on_retry_scheduled.as_ref()) {
            cb((attempt, max_attempts, delay_ms, error_message.clone())).await;
        }

        // Normalize aborts during retry backoff to the same shape as provider
        // stream aborts.
        if !abortable_sleep(delay_ms, signal).await {
            if let Some(cb) = callbacks.and_then(|c| c.on_retry_finished.as_ref()) {
                cb((false, attempt, Some(error_message))).await;
            }
            let mut aborted = response;
            aborted.stop_reason = StopReason::Aborted;
            aborted.error_message = None;
            return aborted;
        }
        if let Some(cb) = callbacks.and_then(|c| c.on_retry_attempt_start.as_ref()) {
            cb(()).await;
        }
    }
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

/// `isRetryableAssistantError`: classifies whether a failed assistant message
/// looks like a transient provider/transport error.
pub fn is_retryable_assistant_error(message: &AssistantMessage) -> bool {
    if message.stop_reason != StopReason::Error {
        return false;
    }
    let Some(error_message) = &message.error_message else {
        return false;
    };
    if non_retryable_provider_limit_error_pattern().is_match(error_message) {
        return false;
    }
    retryable_provider_error_pattern().is_match(error_message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ApiKind, AssistantRole, Usage};

    fn assistant(stop_reason: StopReason, error_message: Option<&str>) -> AssistantMessage {
        AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![],
            api: ApiKind::from("openai-completions"),
            provider: "openai".to_owned(),
            model: "m".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason,
            error_message: error_message.map(str::to_owned),
            timestamp: 0,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        }
    }

    /// #9669 (e98f287ee @ d1230ea20): Azure peak-load capacity errors are
    /// retryable (full upstream error string).
    #[test]
    fn test_retryable_azure_peak_load_capacity_errors() {
        const AZURE_PEAK_LOAD_ERROR: &str = "The system is currently experiencing high demand and cannot process your request. Your request exceeds the maximum usage size allowed during peak load. For improved capacity reliability, consider switching to Provisioned Throughput.";
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some(AZURE_PEAK_LOAD_ERROR)
        )));
    }

    /// #10278 (3874b3e98) / #10543 (8b5708dbb) / #10379 (5b6c792b4):
    /// capacity, server-busy, and HTTP/2 pending-stream errors retry.
    #[test]
    fn test_retryable_capacity_busy_and_http2_errors() {
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("Selected model is at capacity. Please try again later.")
        )));
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("server_busy")
        )));
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("Servers are currently busy. Please retry.")
        )));
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("The pending stream has been canceled (caused by: connect ETIMEDOUT)")
        )));
        // #10487 (7fb59f995): Mistral's finish_reason "error" message.
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("Provider stopped with: error (server error)")
        )));
        // The non-retryable limit pattern still wins where it matches.
        assert!(!is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("quota exceeded; model is at capacity")
        )));
    }

    #[test]
    fn test_is_retryable_assistant_error_classification() {
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("429 too many requests")
        )));
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("connection reset by peer: connection error")
        )));
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("Server requested 90s retry delay (max: 60s)")
        )));
        // fe10558eb: upstream request buffer exhaustion wording is retryable.
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("Error: exceeded request buffer limit while retrying upstream")
        )));
        // #9627 (e5d18382a): Cloudflare 520 with no body is retryable.
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("520 status code (no body)")
        )));
        // Quota/billing exhaustion: not retryable.
        assert!(!is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("429 insufficient_quota")
        )));
        assert!(!is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("billing hard limit reached; 429")
        )));
        // Unclassified errors are not retryable.
        assert!(!is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("authentication failed")
        )));
        assert!(!is_retryable_assistant_error(&assistant(
            StopReason::Stop,
            None
        )));
        assert!(!is_retryable_assistant_error(&assistant(
            StopReason::Error,
            None
        )));
    }

    /// `02eed88fd`: the ChatGPT shared usage limit is a subscription limit
    /// (not retryable); the usage/user unavailable codes are transient.
    #[test]
    fn test_chatgpt_subscription_sharing_error_classification() {
        assert!(!is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("subscription_sharing_usage_limit_exceeded: usage limit reached")
        )));
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("subscription_sharing_usage_unavailable: try again")
        )));
        assert!(is_retryable_assistant_error(&assistant(
            StopReason::Error,
            Some("subscription_sharing_user_unavailable")
        )));
    }

    #[tokio::test]
    async fn test_retry_assistant_call_success_first_try() {
        let policy = RetryPolicy {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 1,
            max_agent_delay_ms: None,
        };
        let response = retry_assistant_call(
            || async { assistant(StopReason::Stop, None) },
            Some(&policy),
            None,
            None,
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Stop);
    }

    #[tokio::test]
    async fn test_retry_assistant_call_retries_then_succeeds() {
        let policy = RetryPolicy {
            enabled: true,
            max_retries: 2,
            base_delay_ms: 1,
            max_agent_delay_ms: None,
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let calls2 = calls.clone();
        let response = retry_assistant_call(
            move || {
                let calls = calls2.clone();
                async move {
                    let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n < 2 {
                        assistant(StopReason::Error, Some("503 service unavailable"))
                    } else {
                        assistant(StopReason::Stop, None)
                    }
                }
            },
            Some(&policy),
            None,
            None,
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Stop);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_assistant_call_non_retryable_fails_fast() {
        let policy = RetryPolicy {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 1,
            max_agent_delay_ms: None,
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let calls2 = calls.clone();
        let response = retry_assistant_call(
            move || {
                let calls = calls2.clone();
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    assistant(StopReason::Error, Some("insufficient_quota"))
                }
            },
            Some(&policy),
            None,
            None,
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Error);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_retry_assistant_call_abort_during_backoff() {
        let policy = RetryPolicy {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 10_000,
            max_agent_delay_ms: None,
        };
        let token = CancellationToken::new();
        let token2 = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            token2.cancel();
        });
        let response = retry_assistant_call(
            || async { assistant(StopReason::Error, Some("503")) },
            Some(&policy),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Aborted);
        assert_eq!(response.error_message, None);
        // 86bac52f9 (retry.ts:203-207): the aborted message is the destructure
        // `{ errorMessage: _, ...rest }` — serde must omit the key entirely,
        // not serialize a JSON null.
        let serialized = serde_json::to_value(&response).expect("serialize");
        assert!(
            serialized.get("errorMessage").is_none(),
            "errorMessage must be absent from the wire shape: {serialized}"
        );
        assert_eq!(serialized["stopReason"], "aborted");
    }

    #[tokio::test]
    async fn test_retry_assistant_call_aborted_response_never_retried() {
        let policy = RetryPolicy {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 1,
            max_agent_delay_ms: None,
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let calls2 = calls.clone();
        let response = retry_assistant_call(
            move || {
                let calls = calls2.clone();
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    assistant(StopReason::Aborted, Some("Operation aborted"))
                }
            },
            Some(&policy),
            None,
            None,
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Aborted);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    // -----------------------------------------------------------------------
    // #8826 (c37b0e03b "cap agent retry backoff") — upstream retry.test.ts
    // -----------------------------------------------------------------------

    /// `retryDelayMs caps agent retry delay` (upstream table).
    #[test]
    fn test_retry_delay_ms_caps_agent_delay_8826() {
        // Default cap (60s): base 2s, attempt 6 → 64s uncapped → 60s.
        assert_eq!(retry_delay_ms(2_000, None, 6), 60_000);
        // Explicit cap 5s: attempt 5 → 32s uncapped → 5s.
        assert_eq!(retry_delay_ms(2_000, Some(5_000), 5), 5_000);
        // Cap 0 disables waiting entirely.
        assert_eq!(retry_delay_ms(2_000, Some(0), 5), 0);
        // Below the cap the exponential value passes through unchanged.
        assert_eq!(retry_delay_ms(2_000, None, 3), 8_000);
        // attempt 0/1 clamp to the first power (2^(max(0, attempt-1))).
        assert_eq!(retry_delay_ms(2_000, None, 0), 2_000);
    }

    /// `reports capped retry delays` (upstream): onRetryScheduled observes
    /// the capped schedule [10, 15, 15, 15] for base 10 / cap 15 / 4 retries.
    #[tokio::test(start_paused = true)]
    async fn test_retry_assistant_call_reports_capped_retry_delays_8826() {
        let policy = RetryPolicy {
            enabled: true,
            max_retries: 4,
            base_delay_ms: 10,
            max_agent_delay_ms: Some(15),
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let calls2 = calls.clone();
        let scheduled: std::sync::Arc<std::sync::Mutex<Vec<u64>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let scheduled2 = scheduled.clone();
        let callbacks = RetryCallbacks {
            on_retry_scheduled: Some(Box::new(
                move |(_, _, delay_ms, _): (u32, u32, u64, String)| {
                    let scheduled = scheduled2.clone();
                    Box::pin(async move {
                        scheduled
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(delay_ms);
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                },
            )),
            ..Default::default()
        };
        let response = retry_assistant_call(
            move || {
                let calls = calls2.clone();
                async move {
                    let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if n < 5 {
                        assistant(StopReason::Error, Some("terminated"))
                    } else {
                        assistant(StopReason::Stop, None)
                    }
                }
            },
            Some(&policy),
            None,
            Some(&callbacks),
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Stop);
        assert_eq!(
            *scheduled.lock().unwrap_or_else(|e| e.into_inner()),
            vec![10, 15, 15, 15]
        );
    }
}
