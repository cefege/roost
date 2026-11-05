//! The browser's only door to a worker's native OMP stats dashboard.
//!
//! A capability URL is minted from an admitted native `/stats` host service,
//! and this route forwards its relative requests to that exact worker loopback
//! origin. The page is served with an opaque-origin CSP sandbox: it can use its
//! own assets and data, and it cannot read this application's storage, opener,
//! or cookies. The capability token in the path is the authorization; no
//! caller credential is forwarded and no upstream cookie reaches the browser.
//!
//! `/api/events` is the one stream. A worker link carries request/reply frames,
//! not a byte stream, so the route polls the native status endpoint at the same
//! cadence the dashboard expects and emits ordinary server-sent events.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use roost_protocol::native_dashboard::{
    HttpHeader, HttpRequest, HttpResponse, MAX_REQUEST_BODY_BYTES, MAX_REQUEST_PATH_BYTES,
    MAX_RESPONSE_BODY_BYTES, capability_path, parse_capability_path, sanitize_request_headers,
};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

use crate::agent::omp::{DashboardCapability, DashboardRequestAdmission, DashboardRequestLease};
use crate::http::listener::ListenerState;
use crate::serve::now_ms;
use crate::workers::send::{SendOutcome, send_frame_through};

mod stream;

const FORWARD_TIMEOUT: Duration = Duration::from_secs(25);
/// One non-stream capability request's whole budget: the browser's upload and
/// the worker's answer share it, so a stalled body cannot hold a request slot
/// indefinitely.
const REQUEST_LIFETIME: Duration = Duration::from_secs(30);

/// Serve one capability URL.
pub async fn serve(State(state): State<Arc<ListenerState>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let Some((token, upstream)) = parse_capability_path(&path) else {
        return not_found();
    };
    if upstream == "/" {
        let canonical = capability_path(token);
        if path != canonical {
            return Redirect::permanent(&canonical).into_response();
        }
    }
    if upstream.len() > MAX_REQUEST_PATH_BYTES {
        return (StatusCode::URI_TOO_LONG, "dashboard path too long").into_response();
    }
    let now = now_ms();
    let Some(capability) = state.services.omp.dashboards.authorize(token, now) else {
        return not_found();
    };
    if !state
        .services
        .omp
        .dashboard_binding_is_current(&capability)
        .await
    {
        return not_found();
    }
    let Some(origin) = public_origin(&request) else {
        return (StatusCode::BAD_REQUEST, "missing host").into_response();
    };
    let full_path = match request.uri().query() {
        Some(query) => format!("{upstream}?{query}"),
        None => upstream.to_owned(),
    };
    if request.method() == axum::http::Method::GET
        && upstream == "/api/events"
        && request.uri().query().is_none()
    {
        let Some(lease) = state.services.omp.dashboards.begin_stream(token, now) else {
            return not_found();
        };
        return stream::event_stream(
            Arc::clone(&state.services),
            capability,
            token.to_owned(),
            origin,
            lease,
        );
    }
    let admission = state.services.omp.dashboards.begin_request(token, now_ms());
    match admission {
        DashboardRequestAdmission::Admitted(lease) => {
            forward(
                Arc::clone(&state.services),
                capability,
                request,
                full_path,
                origin,
                token.to_owned(),
                lease,
            )
            .await
        }
        DashboardRequestAdmission::Saturated => too_many_requests(),
        DashboardRequestAdmission::Missing => not_found(),
    }
}

/// The outcome of reading a capability request body under its absolute
/// deadline.
enum BodyRead {
    Body(Bytes),
    TooLarge,
    TimedOut,
}

/// Read the browser's body within what is left of the request lifetime.
async fn read_body(body: Body, deadline: tokio::time::Instant) -> BodyRead {
    match tokio::time::timeout_at(
        deadline,
        axum::body::to_bytes(body, MAX_REQUEST_BODY_BYTES + 1),
    )
    .await
    {
        Ok(Ok(body)) if body.len() <= MAX_REQUEST_BODY_BYTES => BodyRead::Body(body),
        Ok(_) => BodyRead::TooLarge,
        Err(_) => BodyRead::TimedOut,
    }
}

/// Forward one non-stream request through the worker proxy.
///
/// `_lease` holds the capability's request slot for the whole body read and
/// worker call; every return path, timeout, and cancellation drops it.
async fn forward(
    services: Arc<crate::services::CoordServices>,
    capability: DashboardCapability,
    request: Request,
    full_path: String,
    origin: String,
    token: String,
    _lease: DashboardRequestLease,
) -> Response {
    let deadline = tokio::time::Instant::now() + REQUEST_LIFETIME;
    let method = request.method().as_str().to_owned();
    let headers = sanitize_request_headers(
        request
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value.to_str().ok().map(|value| HttpHeader {
                    name: name.as_str().to_owned(),
                    value: value.to_owned(),
                })
            })
            .collect(),
    );
    let body = match read_body(request.into_body(), deadline).await {
        BodyRead::Body(body) => body,
        BodyRead::TooLarge => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "dashboard request too large").into_response();
        }
        BodyRead::TimedOut => return request_timeout(),
    };
    if !services.omp.dashboards.is_live(&token, now_ms()) {
        return not_found();
    }
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return request_timeout();
    }
    let request = HttpRequest {
        method,
        path: full_path,
        headers,
        body_base64: if body.is_empty() {
            String::new()
        } else {
            STANDARD.encode(body)
        },
    };
    match worker_request(
        &services,
        &capability,
        request,
        FORWARD_TIMEOUT.min(remaining),
    )
    .await
    {
        Ok(response) => capability_response(response, &origin),
        Err(WorkerRequestError::TimedOut) => gateway_timeout(),
        Err(WorkerRequestError::Failed(error)) => gateway_error(error),
    }
}

/// Why a worker request did not produce a response.
enum WorkerRequestError {
    TimedOut,
    Failed(String),
}

/// Ask the owning worker for one native dashboard request.
async fn worker_request(
    services: &Arc<crate::services::CoordServices>,
    capability: &DashboardCapability,
    request: HttpRequest,
    timeout: Duration,
) -> Result<HttpResponse, WorkerRequestError> {
    let worker_fp = WorkerFp::try_from(capability.worker_fp.clone()).map_err(|_| {
        WorkerRequestError::Failed("the dashboard worker fingerprint is invalid".to_owned())
    })?;
    let handle = services
        .workers
        .current_routable(&worker_fp)
        .ok_or_else(|| WorkerRequestError::Failed("the dashboard worker is offline".to_owned()))?;
    let pending = services
        .scrollback
        .pending()
        .create_fresh(Some(capability.worker_fp.as_str()), now_ms())
        .map_err(|_| {
            WorkerRequestError::Failed("the dashboard correlation table is full".to_owned())
        })?;
    let frame = ClientControlFrame::StatsDashboardHttp {
        request_id: pending.request_id().to_owned(),
        host: capability.target.host.clone(),
        port: capability.target.port,
        method: request.method,
        path: request.path,
        headers: request.headers,
        body_base64: request.body_base64,
        trace_id: None,
    };
    let downstream = CoordWorkerDownstream::BrowserCommand {
        browser_id: capability.service_id.clone(),
        viewer_id: capability.conversation_id.clone(),
        request_id: pending.request_id().to_owned(),
        frame,
        trace_id: None,
    };
    match send_frame_through(services.workers.as_ref(), &handle, downstream) {
        SendOutcome::Admitted { .. } => {}
        SendOutcome::Refused(refusal) => {
            return Err(WorkerRequestError::Failed(format!(
                "the dashboard request did not reach its worker: {refusal}"
            )));
        }
    }
    let mut pending = pending;
    let raw = tokio::time::timeout(timeout, pending.settle())
        .await
        .map_err(|_| WorkerRequestError::TimedOut)?
        .map_err(|error| {
            WorkerRequestError::Failed(format!("the native dashboard request failed: {error}"))
        })?;
    serde_json::from_value::<HttpResponse>(raw).map_err(|_| {
        WorkerRequestError::Failed("the native dashboard answered a malformed response".to_owned())
    })
}

/// The response headers every capability response starts with.
struct HeaderMapBuilder {
    headers: axum::http::HeaderMap,
}

impl HeaderMapBuilder {
    fn insert(&mut self, name: header::HeaderName, value: HeaderValue) {
        self.headers.insert(name, value);
    }

    fn new(origin: &str) -> Self {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        headers.insert(
            header::HeaderName::from_static("referrer-policy"),
            HeaderValue::from_static("no-referrer"),
        );
        headers.insert(
            header::HeaderName::from_static("x-roost-opaque-capability"),
            HeaderValue::from_static("1"),
        );
        if let Ok(value) = HeaderValue::from_str(&sandbox_csp(origin)) {
            headers.insert(header::CONTENT_SECURITY_POLICY, value);
        }
        Self { headers }
    }
}

/// A native response, with the bridge's own headers replacing anything the
/// dashboard said about caching or framing.
fn capability_response(response: HttpResponse, origin: &str) -> Response {
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = if response.body_base64.is_empty() {
        Vec::new()
    } else {
        match STANDARD.decode(&response.body_base64) {
            Ok(body) if body.len() <= MAX_RESPONSE_BODY_BYTES => body,
            _ => return gateway_error("the dashboard response exceeded its bound"),
        }
    };
    let mut builder = HeaderMapBuilder::new(origin);
    for header in response.headers {
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(header.name.as_bytes()),
            HeaderValue::from_str(&header.value),
        ) {
            builder.headers.insert(name, value);
        }
    }
    builder
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    *response.headers_mut() = builder.headers;
    response
}

/// The sandbox is the isolation: no `allow-same-origin`, so the page has an
/// opaque origin and cannot read this application's storage or its opener.
fn sandbox_csp(origin: &str) -> String {
    let http = format!("http://{origin}");
    let https = format!("https://{origin}");
    format!(
        "sandbox allow-scripts allow-forms allow-downloads allow-modals; \
         default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' 'self' {http} {https}; \
         style-src 'unsafe-inline' 'self' {http} {https}; \
         img-src 'self' {http} {https} data: blob:; \
         font-src 'self' {http} {https} data:; \
         connect-src 'self' {http} {https}; \
         worker-src 'none'; object-src 'none'; base-uri 'none'; \
         form-action 'none'; frame-ancestors 'none'"
    )
}

/// The request's public authority, used only to scope `connect-src` back to
/// this same coordinator.
fn public_origin(request: &Request) -> Option<String> {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())?
        .trim()
        .to_owned();
    if host.is_empty()
        || host.bytes().any(|byte| {
            byte <= b' '
                || byte == 0x7f
                || !(byte.is_ascii_alphanumeric()
                    || matches!(byte, b'.' | b'-' | b':' | b'[' | b']'))
        })
    {
        return None;
    }
    Some(host)
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "dashboard capability not found").into_response()
}

fn request_timeout() -> Response {
    (StatusCode::REQUEST_TIMEOUT, "dashboard request timed out").into_response()
}

fn gateway_timeout() -> Response {
    (
        StatusCode::GATEWAY_TIMEOUT,
        "the native dashboard did not answer in time",
    )
        .into_response()
}

fn too_many_requests() -> Response {
    (StatusCode::TOO_MANY_REQUESTS, "too many dashboard requests").into_response()
}

fn gateway_error(message: impl Into<String>) -> Response {
    (StatusCode::BAD_GATEWAY, message.into()).into_response()
}

#[cfg(test)]
mod tests;
