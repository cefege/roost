//! The security-header and CORS response layers, and the preflight they answer.
//!
//! Owned by the middleware stack; `http::listener` mounts [`security_layer`]
//! below the admission gate. Port of `apps/coord/src/middleware/security.ts`
//! and of the one CSP builder both roost HTTP doors share
//! (`packages/host/src/http-security.ts`).
//!
//! NONE OF THIS IS AN AUTHORIZATION DECISION. The admission gate above has
//! already refused every request whose `Origin` is not on the allowlist, so
//! these headers describe the response to a caller that was already admitted --
//! which is why an origin that is not allowed gets no
//! `Access-Control-Allow-Origin` here rather than a refusal.
//!
//! HSTS FOLLOWS `trust_proxy`, never the scheme of the connection
//! (`bun-coordinator-listeners.ts:350`): the coordinator never terminates TLS
//! (`listener.rs` module header), so only a front door that the operator trusts
//! to terminate it can make the header true rather than a downgrade instruction
//! to a browser that reached the loopback bind directly.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use roost_protocol::wire::headers::X_ROOST_AUTH_LAYER;

/// The two origins the transcription client is allowed to reach, in the order
/// the CSP lists them (`security.ts:29-31`).
const TRANSCRIPTION_ORIGINS: [&str; 2] = ["https://api.deepgram.com", "wss://api.deepgram.com"];

/// The tail every CSP ends with: this coordinator serves no frame anyone may
/// embed, and a page it served is not a page an attacker may frame to clickjack
/// an operator's approval dialog.
const CSP_TAIL: &str = "frame-ancestors 'none'";

/// The response header names this module writes, as the shared contract spells
/// them, so a rename on one side cannot silently drop the other's guarantee.
const CONTENT_SECURITY_POLICY: &str = "content-security-policy";
const X_FRAME_OPTIONS: &str = "x-frame-options";
const X_CONTENT_TYPE_OPTIONS: &str = "x-content-type-options";
const REFERRER_POLICY: &str = "referrer-policy";
const PERMISSIONS_POLICY: &str = "permissions-policy";
const STRICT_TRANSPORT_SECURITY: &str = "strict-transport-security";

/// Every response header value this module writes, built once at boot.
///
/// A CSP is ~10 joined sources; assembling it per response is a string
/// allocation on the path of every Connect call, and the values cannot change
/// while the process runs because the operator's config cannot.
#[derive(Debug)]
struct SecurityHeaders {
    csp: HeaderValue,
    frame: HeaderValue,
    content_type: HeaderValue,
    referrer: HeaderValue,
    permissions: HeaderValue,
    hsts: Option<HeaderValue>,
}

/// What the response layers are allowed to say, resolved from the operator's
/// configuration at boot.
#[derive(Debug)]
pub struct SecurityOptions {
    /// Bare origins whose `Origin` header is echoed back.
    cors_allowed_origins: Vec<String>,
    /// The prebuilt response header values.
    headers: SecurityHeaders,
}

/// Resolve the security options a coordinator answers with.
///
/// The worker's loopback door is a first-class browser origin in both
/// directions: its SPA's fetches carry a bearer credential and no cookies, so
/// allowing it for CORS grants reachability only, and a page this coordinator
/// served dials that door's terminal socket, so it belongs in `connect-src`
/// too (`security.ts:22-25`).
#[must_use]
pub fn security_options_for_config(config: &roost_host::CoordConfig) -> SecurityOptions {
    let mut cors_allowed_origins = vec![roost_host::DEFAULT_WORKER_LOCAL_UI_ORIGIN.to_owned()];
    cors_allowed_origins.extend(config.cors_allowed_origins.iter().cloned());

    let mut connect_origins: Vec<String> = TRANSCRIPTION_ORIGINS
        .iter()
        .map(|origin| (*origin).to_owned())
        .collect();
    for declared in [
        config.public_url.as_deref(),
        config.web_public_url.as_deref(),
        Some(roost_host::DEFAULT_WORKER_LOCAL_UI_ORIGIN),
    ]
    .into_iter()
    .flatten()
    .chain(config.cors_allowed_origins.iter().map(String::as_str))
    {
        if declared.is_empty() {
            continue;
        }
        connect_origins.push(declared.to_owned());
        // Every browser transport this page opens has a WebSocket twin: the Sync
        // socket to this coordinator, and a terminal socket to a worker's
        // loopback door. That door is plaintext, so the twin has to be derived
        // from either scheme rather than from the page's own.
        connect_origins.push(websocket_twin(declared));
    }

    SecurityOptions {
        cors_allowed_origins,
        headers: SecurityHeaders {
            csp: header_value(&build_csp(config.relaxed_csp, &connect_origins)),
            frame: HeaderValue::from_static("DENY"),
            content_type: HeaderValue::from_static("nosniff"),
            referrer: HeaderValue::from_static("no-referrer"),
            permissions: HeaderValue::from_static("camera=(), geolocation=(), microphone=(self)"),
            hsts: config
                .trust_proxy
                .then(|| HeaderValue::from_static("max-age=31536000")),
        },
    }
}

/// The `ws://` or `wss://` twin of an origin.
///
/// Derived rather than declared because a browser applies `connect-src` to the
/// WebSocket scheme too, and a page that may reach `https://door` but not
/// `wss://door` opens a socket and gets a CSP violation.
fn websocket_twin(origin: &str) -> String {
    if let Some(twin) = origin.strip_prefix("https://") {
        return format!("wss://{twin}");
    }
    if let Some(twin) = origin.strip_prefix("http://") {
        return format!("ws://{twin}");
    }
    origin.to_owned()
}

/// The `Content-Security-Policy` this coordinator answers with.
#[must_use]
pub fn build_csp(relaxed: bool, connect_origins: &[String]) -> String {
    let mut connections: Vec<&str> = vec!["'self'"];
    for origin in connect_origins {
        if !connections.contains(&origin.as_str()) {
            connections.push(origin);
        }
    }
    // A relaxed policy admits plaintext endpoints, which is what a local-first
    // coordinator serving `http://127.0.0.1:<port>` needs. It is opt-in
    // because it also admits every other plaintext endpoint, and the operator
    // asked for one specific door rather than the class.
    if relaxed {
        connections.push("http:");
        connections.push("ws:");
    }
    format!(
        "default-src 'self'; \
         script-src 'self' 'wasm-unsafe-eval' blob:; \
         worker-src 'self' blob:; \
         style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob:; \
         font-src 'self' data:; \
         base-uri 'self'; \
         form-action 'none'; \
         object-src 'none'; \
         connect-src {}; \
         {CSP_TAIL}",
        connections.join(" ")
    )
}

/// Stamp the CORS headers for one request's origin.
///
/// `Vary` is unconditional and the method/header permissions are unconditional
/// too (`security.ts:65-67`): a cache that stored one response without them
/// would hand a later origin a decision made for an earlier one.
pub fn apply_cors(headers: &mut HeaderMap, request_origin: Option<&str>, allowed: &[String]) {
    if let Some(allowed) = request_origin.filter(|origin| allowed.iter().any(|it| it == origin))
        && let Ok(origin) = HeaderValue::from_str(allowed)
    {
        headers.insert(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(
            axum::http::header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static(X_ROOST_AUTH_LAYER),
        );
    }
    headers.insert(
        axum::http::header::VARY,
        HeaderValue::from_static(
            "origin, access-control-request-method, access-control-request-headers",
        ),
    );
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
}

/// Stamp the security headers on one response.
pub fn apply_security_headers(headers: &mut HeaderMap, options: &SecurityOptions) {
    let prebuilt = &options.headers;
    for (name, value) in [
        (CONTENT_SECURITY_POLICY, Some(&prebuilt.csp)),
        (X_FRAME_OPTIONS, Some(&prebuilt.frame)),
        (X_CONTENT_TYPE_OPTIONS, Some(&prebuilt.content_type)),
        (REFERRER_POLICY, Some(&prebuilt.referrer)),
        (PERMISSIONS_POLICY, Some(&prebuilt.permissions)),
        (STRICT_TRANSPORT_SECURITY, prebuilt.hsts.as_ref()),
    ] {
        if let (Ok(name), Some(value)) = (HeaderName::from_bytes(name.as_bytes()), value) {
            headers.insert(name, value.clone());
        }
    }
}

/// The `204` a CORS preflight is answered with, carrying the same headers every
/// other response carries.
///
/// A preflight is not a route: it is the browser asking whether the real
/// request would be allowed, and answering it from the route table would make
/// the answer depend on which path the real request happens to use.
#[must_use]
pub fn preflight_response(request_origin: Option<&str>, options: &SecurityOptions) -> Response {
    let mut headers = HeaderMap::new();
    apply_cors(&mut headers, request_origin, &options.cors_allowed_origins);
    apply_security_headers(&mut headers, options);
    (StatusCode::NO_CONTENT, headers).into_response()
}

/// Answer preflights and decorate every other response.
pub async fn security_layer(
    State(options): State<Arc<SecurityOptions>>,
    request: Request,
    next: Next,
) -> Response {
    let request_origin = request
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if request.method() == Method::OPTIONS {
        return preflight_response(request_origin.as_deref(), &options);
    }
    let mut response = next.run(request).await;
    apply_cors(
        response.headers_mut(),
        request_origin.as_deref(),
        &options.cors_allowed_origins,
    );
    apply_security_headers(response.headers_mut(), &options);
    response
}

/// The origins whose `Origin` header this coordinator echoes back.
#[must_use]
pub fn cors_allowed_origins(options: &SecurityOptions) -> &[String] {
    &options.cors_allowed_origins
}

fn header_value(raw: &str) -> HeaderValue {
    HeaderValue::from_str(raw).unwrap_or_else(|_| {
        // A CSP this module built cannot contain a byte a header value rejects,
        // so a refusal here would be a defect rather than operator input. The
        // fallback is a policy that permits nothing, never one that permits
        // everything.
        tracing::error!("the built content-security-policy is not a header value");
        HeaderValue::from_static("default-src 'none'")
    })
}
