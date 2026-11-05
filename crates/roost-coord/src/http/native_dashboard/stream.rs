//! The capability's server-sent-event bridge.
//!
//! The worker link is request/reply, so this module turns the native
//! `/api/events` stream into bounded one-second status polls. Every poll spends
//! the capability's request window, and the held lease bounds concurrent
//! streams and releases the slot when the response body is dropped.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::stream::unfold;
use roost_protocol::native_dashboard::{HttpRequest, MAX_RESPONSE_BODY_BYTES};
use tokio::time::{Interval, MissedTickBehavior};

use crate::agent::omp::{DashboardCapability, DashboardStreamLease};
use crate::serve::now_ms;

use super::{HeaderMapBuilder, worker_request};

const STATUS_TIMEOUT: Duration = Duration::from_secs(8);
const STATUS_POLL: Duration = Duration::from_secs(1);
const MAX_STREAM_FAILURES: u32 = 3;
const STREAM_HEARTBEAT_TICKS: u64 = 15;
const STREAM_BINDING_CHECK_TICKS: u64 = 5;

/// The status stream a sandboxed page reads in place of the native SSE route.
pub(super) fn event_stream(
    services: Arc<crate::services::CoordServices>,
    capability: DashboardCapability,
    token: String,
    origin: String,
    lease: DashboardStreamLease,
) -> Response {
    let mut interval = tokio::time::interval(STATUS_POLL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let response_origin = origin.clone();
    let state = StatusStream {
        services,
        capability,
        token,
        _lease: lease,
        interval,
        last: None,
        failures: 0,
        ticks: 0,
    };
    let stream = unfold(state, |mut state| async move {
        loop {
            state.interval.tick().await;
            state.ticks = state.ticks.saturating_add(1);
            let now = now_ms();
            state.services.omp.dashboards.authorize(&state.token, now)?;
            if state.ticks % STREAM_BINDING_CHECK_TICKS == 0
                && !state
                    .services
                    .omp
                    .dashboard_binding_is_current(&state.capability)
                    .await
            {
                return None;
            }
            let request = HttpRequest {
                method: "GET".into(),
                path: "/api/status".into(),
                headers: Vec::new(),
                body_base64: String::new(),
            };
            match worker_request(&state.services, &state.capability, request, STATUS_TIMEOUT).await
            {
                Ok(response) if response.status == 200 => {
                    let Ok(body) = STANDARD.decode(response.body_base64) else {
                        state.failures = state.failures.saturating_add(1);
                        continue;
                    };
                    if body.len() > MAX_RESPONSE_BODY_BYTES {
                        state.failures = state.failures.saturating_add(1);
                        continue;
                    }
                    let body = String::from_utf8_lossy(&body).into_owned();
                    if state.last.as_deref() == Some(body.as_str()) {
                        if state.ticks % STREAM_HEARTBEAT_TICKS == 0 {
                            return Some((
                                Ok::<Bytes, std::convert::Infallible>(Bytes::from_static(
                                    b": keep-alive\n\n",
                                )),
                                state,
                            ));
                        }
                        continue;
                    }
                    state.failures = 0;
                    state.last = Some(body.clone());
                    return Some((Ok(Bytes::from(format!("data: {body}\n\n"))), state));
                }
                _ => {
                    state.failures = state.failures.saturating_add(1);
                    if state.failures >= MAX_STREAM_FAILURES {
                        return None;
                    }
                }
            }
        }
    });
    let mut headers = HeaderMapBuilder::new(&response_origin);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = StatusCode::OK;
    *response.headers_mut() = headers.headers;
    response
}

struct StatusStream {
    services: Arc<crate::services::CoordServices>,
    capability: DashboardCapability,
    token: String,
    /// Held for the stream's lifetime; dropping the stream releases the slot.
    _lease: DashboardStreamLease,
    interval: Interval,
    last: Option<String>,
    failures: u32,
    ticks: u64,
}
