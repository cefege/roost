//! The rate-limit mount: one request budget per client per window, spent only
//! by the methods [`crate::middleware::rate_limit`] names.
//!
//! Owned by the middleware stack; `http::listener` mounts [`rate_limit_layer`]
//! below the CORS layer and above the audit mount. The budget itself, the
//! bucket table and the list of limited methods all live in
//! `middleware::rate_limit`; this module resolves WHO is spending and WHAT for,
//! and answers the refusal.
//!
//! THE KEY IS THE CALLER ORIGIN, NOT THE CONNECTION. The layer reads the
//! profile `middleware::caller_origin` resolved, which is the listener's
//! observed peer on a direct bind and the proxy's first `X-Forwarded-For` entry
//! on a trusted one. A key of the connection itself would hand every reconnect
//! a fresh budget and the limit would never fire.
//!
//! ONLY CONNECT PATHS SPEND A BUDGET, and that falls out of the method name: the
//! limiter knows 34 method names and returns "admitted" for everything else. The
//! two WebSocket upgrades are long-lived sockets rather than request budgets --
//! a 100/minute budget on one would break a terminal instead of protecting
//! anything -- and they are not Connect paths, so they are never limited.
//!
//! THE LAYER CARRIES ITS OWN STATE, never `http::listener::ListenerState`. A
//! middleware layer runs in front of the routes, and the listener state belongs
//! to the handlers behind it; what this layer needs is the process state the
//! budget table lives in.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header::RETRY_AFTER};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::http::listener::CONNECT_PATH_PREFIX;
use crate::middleware::caller_origin::{UNKNOWN_PEER, from_extensions};
use crate::middleware::rate_limit::RateLimitCaller;
use crate::services::CoordServices;

/// The refusal body, byte for byte what v2 answers
/// (`apps/coord/src/middleware/rate-limit.ts`).
const REFUSAL_BODY: &str = r#"{"error":"rate limit exceeded"}"#;

/// The bare Connect method name this path names, or `None` for a path that is
/// not a Connect method at all.
///
/// The bare name is the whole key, and `RATE_LIMITED_METHODS` is the single
/// list of them: naming a second set of methods here would be a second answer to
/// "which methods are limited", and the two would drift.
fn method_name(path: &str) -> Option<&str> {
    path.strip_prefix(CONNECT_PATH_PREFIX)
        .filter(|method| !method.is_empty())
}

/// The address this request spends from, as the listener's trust profile named
/// it. A request with no resolved profile spends from the shared unknown budget
/// rather than from a guess.
fn spending_client(extensions: &axum::http::Extensions) -> &str {
    from_extensions(extensions).map_or(UNKNOWN_PEER, |origin| origin.client_ip.as_str())
}

/// Spend one budget for this request, and refuse it when the budget is gone.
pub async fn rate_limit_layer(
    State(services): State<Arc<CoordServices>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(method) = method_name(request.uri().path()) else {
        return next.run(request).await;
    };
    let client_ip = spending_client(request.extensions()).to_owned();
    let caller = RateLimitCaller::from_client_ip(client_ip.clone());
    let Some(refusal) = services.rate_limit.admit(method, &caller) else {
        return next.run(request).await;
    };
    // The limiter already logs this refusal, deduped to once per window per
    // caller. A second line here would undo that dedup for a client retrying in
    // a loop, which is the case the dedup exists for.
    refusal_response(refusal.retry_after_seconds)
}

/// The `429` a spent budget answers with.
///
/// `Retry-After` is the limiter's own clamped value, between one second and the
/// window, so it goes into the header as it stands: clamping again here could
/// only widen it, and a zero would tell a client to retry immediately, which is
/// the one outcome the clamp exists to prevent.
fn refusal_response(retry_after_seconds: u64) -> Response {
    let retry_after = HeaderValue::from_str(&retry_after_seconds.to_string())
        .unwrap_or_else(|_| HeaderValue::from_static("60"));
    (
        StatusCode::TOO_MANY_REQUESTS,
        [
            (axum::http::header::CONTENT_TYPE, "application/json"),
            (RETRY_AFTER, retry_after),
        ],
        REFUSAL_BODY,
    )
        .into_response()
}
