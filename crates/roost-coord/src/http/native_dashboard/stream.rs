//! The authenticated capability's real upstream SSE body.
//!
//! Bounded worker stream leases preserve bytes and browser backpressure. Drop
//! closes the actual worker response, including cancellation mid-read.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::response::Response;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::stream::unfold;
use roost_protocol::native_dashboard::{
    HttpRequest, MAX_RESPONSE_BODY_BYTES, StreamRequest, token_is_well_formed,
};

use crate::agent::omp::{DashboardCapability, DashboardStreamLease};
use crate::serve::now_ms;

use super::{
    WorkerRequestError, capability_response, gateway_error, gateway_timeout, worker_request,
};

const STREAM_TIMEOUT: Duration = Duration::from_secs(25);

pub(super) async fn event_stream(
    services: Arc<crate::services::CoordServices>,
    capability: DashboardCapability,
    token: String,
    origin: String,
    lease: DashboardStreamLease,
) -> Response {
    let mut opened = match worker_request(
        &services,
        &capability,
        request(StreamRequest::Open),
        STREAM_TIMEOUT,
    )
    .await
    {
        Ok(response) => response,
        Err(WorkerRequestError::TimedOut) => return gateway_timeout(),
        Err(WorkerRequestError::Failed(error)) => return gateway_error(error),
    };
    let Some(stream) = opened.stream.take().filter(|stream| {
        token_is_well_formed(&stream.id) && !stream.finished && opened.status == 200
    }) else {
        return gateway_error("the worker did not open its native event stream");
    };
    let state = UpstreamStream {
        services,
        capability,
        token,
        id: stream.id,
        finished: false,
        _lease: lease,
    };
    let stream = unfold(state, |mut state| async move {
        if state.finished {
            return None;
        }
        state
            .services
            .omp
            .dashboards
            .authorize(&state.token, now_ms())?;
        if !state
            .services
            .omp
            .dashboard_binding_is_current(&state.capability)
            .await
        {
            return None;
        }
        let response = worker_request(
            &state.services,
            &state.capability,
            request(StreamRequest::Next {
                id: state.id.clone(),
            }),
            STREAM_TIMEOUT,
        )
        .await
        .ok()?;
        let status = response.stream?;
        if response.status != 200 || status.id != state.id {
            return None;
        }
        state.finished = status.finished;
        let body = STANDARD.decode(response.body_base64).ok()?;
        if body.len() > MAX_RESPONSE_BODY_BYTES {
            return None;
        }
        Some((
            Ok::<Bytes, std::convert::Infallible>(Bytes::from(body)),
            state,
        ))
    });
    let mut response = capability_response(opened, &origin);
    *response.body_mut() = Body::from_stream(stream);
    response
}

struct UpstreamStream {
    services: Arc<crate::services::CoordServices>,
    capability: DashboardCapability,
    token: String,
    id: String,
    finished: bool,
    _lease: DashboardStreamLease,
}

impl Drop for UpstreamStream {
    fn drop(&mut self) {
        let services = Arc::clone(&self.services);
        let capability = self.capability.clone();
        let id = self.id.clone();
        tokio::spawn(async move {
            let _ = worker_request(
                &services,
                &capability,
                request(StreamRequest::Close { id }),
                STREAM_TIMEOUT,
            )
            .await;
        });
    }
}

fn request(stream: StreamRequest) -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        path: "/api/events".into(),
        headers: Vec::new(),
        body_base64: String::new(),
        stream: Some(stream),
    }
}
