//! Worker HTTP for registered OMP stats resources, never arbitrary local ports.
//!
//! ACP child ownership and the server's random identity must both match before
//! bytes leave the worker. Streams relay the SDK's actual response body.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use roost_protocol::native_dashboard::{
    HttpHeader, HttpRequest, HttpResponse, MAX_REQUEST_BODY_BYTES, MAX_RESPONSE_BODY_BYTES,
    ResourceKey, StreamRequest, is_dashboard_identity, method_is_allowed, path_is_safe,
    sanitize_response_headers,
};
use roost_protocol::wire::control::ClientControlFrame;

use super::{Answered, Boxed, Command, Deps, Refusal, Reply};
pub mod resources;
mod stream;
mod target;
use resources::{DashboardResources, OwnedResource};
use target::{PinnedTarget, ip_is_locally_assigned, resolve_local_target};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PINNED_TARGETS: usize = 32;

pub trait NativeDashboardProxy: Send + Sync {
    fn is_registered(&self, resource: &ResourceKey, host: &str, port: u16) -> bool;
    fn fetch(
        self: Arc<Self>,
        resource: &ResourceKey,
        host: &str,
        port: u16,
        request: HttpRequest,
    ) -> Boxed<Result<HttpResponse, Refusal>>;
}

pub struct NativeDashboardHttp {
    client: Result<reqwest::Client, reqwest::Error>,
    pinned: Mutex<HashMap<ResourceKey, PinnedTarget>>,
    resources: Arc<DashboardResources>,
}

impl std::fmt::Debug for NativeDashboardHttp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeDashboardHttp")
            .finish_non_exhaustive()
    }
}

impl NativeDashboardHttp {
    pub fn new(resources: Arc<DashboardResources>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .connect_timeout(Duration::from_secs(2))
                .build(),
            pinned: Mutex::new(HashMap::new()),
            resources,
        }
    }

    async fn pin_target(&self, resource: &OwnedResource) -> Result<PinnedTarget, Refusal> {
        if let Some(target) = self
            .pinned
            .lock()
            .map_err(|_| refused("the dashboard pin table is poisoned"))?
            .get(&resource.key)
        {
            return Ok(target.clone());
        }
        let host = resource.target.host.clone();
        let port = resource.target.port;
        let resolved = tokio::time::timeout(
            RESOLVE_TIMEOUT,
            tokio::task::spawn_blocking(move || resolve_local_target(&host, port)),
        )
        .await
        .map_err(|_| refused("resolving the dashboard address timed out"))?
        .map_err(|_| refused("resolving the dashboard address failed"))?
        .ok_or_else(|| refused("the dashboard host is not assigned to this machine"))?;
        let mut pinned = self
            .pinned
            .lock()
            .map_err(|_| refused("the dashboard pin table is poisoned"))?;
        if pinned.len() >= MAX_PINNED_TARGETS {
            pinned.clear();
        }
        pinned.insert(resource.key.clone(), resolved.clone());
        Ok(resolved)
    }

    async fn request(
        &self,
        resource: &Arc<OwnedResource>,
        request: HttpRequest,
    ) -> Result<HttpResponse, Refusal> {
        if !method_is_allowed(&request.method) || !path_is_safe(&request.path) {
            return Err(refused(
                "the dashboard request is outside the bridge contract",
            ));
        }
        if let Some(operation) = &request.stream {
            if request.method != "GET"
                || request.path != "/api/events"
                || !request.body_base64.is_empty()
            {
                return Err(refused("the dashboard stream operation is invalid"));
            }
            match operation {
                StreamRequest::Next { id } => return stream::next(resource, id).await,
                StreamRequest::Close { id } => return stream::close(resource, id),
                StreamRequest::Open => {}
            }
        } else if request.path.split('?').next() == Some("/api/events") {
            return Err(refused(
                "the dashboard event endpoint requires a stream lease",
            ));
        }
        let body = decode_body(&request.body_base64, MAX_REQUEST_BODY_BYTES)?;
        let target = self.pin_target(resource).await?;
        if !ip_is_locally_assigned(target.dial) {
            return Err(refused(
                "the dashboard address is no longer assigned to this machine",
            ));
        }
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| refused("the dashboard method is not an HTTP method"))?;
        let client = self
            .client
            .as_ref()
            .map_err(|_| refused("the dashboard HTTP client could not be initialized"))?;
        let mut builder = client.request(
            method,
            format!("http://{}{}", target.authority(), request.path),
        );
        if request.stream.is_none() {
            builder = builder.timeout(REQUEST_TIMEOUT);
        }
        for header in roost_protocol::native_dashboard::sanitize_request_headers(request.headers) {
            builder = builder.header(header.name, header.value);
        }
        if !body.is_empty() {
            builder = builder.body(body);
        }
        let response = tokio::time::timeout(REQUEST_TIMEOUT, builder.send())
            .await
            .map_err(|_| refused("the native dashboard request timed out"))?
            .map_err(|error| unavailable("the native dashboard request failed", &error))?;
        if response_is_redirect(response.status()) {
            return Err(refused("the native dashboard redirected an HTTP request"));
        }
        if !is_dashboard_identity(
            header_str(response.headers(), "x-omp-stats-dashboard"),
            header_str(response.headers(), "x-omp-stats-hostname"),
            &resource.target.identity_host,
        ) || header_str(response.headers(), "x-omp-stats-resource")
            != Some(resource.key.resource_id.as_str())
        {
            return Err(refused(
                "the target is not this OMP process's registered stats server",
            ));
        }
        let status = response.status().as_u16();
        let headers = sanitize_response_headers(
            response
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
        if matches!(request.stream, Some(StreamRequest::Open)) {
            return stream::open(resource, response, status, headers);
        }
        let body = read_bounded(response, MAX_RESPONSE_BODY_BYTES).await?;
        Ok(HttpResponse {
            status,
            headers,
            body_base64: STANDARD.encode(body),
            stream: None,
        })
    }
}

impl NativeDashboardProxy for NativeDashboardHttp {
    fn is_registered(&self, resource: &ResourceKey, host: &str, port: u16) -> bool {
        self.resources.contains(resource, host, port)
    }
    fn fetch(
        self: Arc<Self>,
        key: &ResourceKey,
        host: &str,
        port: u16,
        request: HttpRequest,
    ) -> Boxed<Result<HttpResponse, Refusal>> {
        let resource = self.resources.get(key, host, port);
        Box::pin(async move {
            let resource = resource
                .ok_or_else(|| refused("the OMP stats resource is no longer registered"))?;
            let mut closed = resource.closed.subscribe();
            if *closed.borrow() {
                return Err(refused("the OMP stats resource is closed"));
            }
            tokio::select! {
                _ = closed.changed() => Err(refused("the OMP stats resource closed during the request")),
                response = self.request(&resource, request) => response,
            }
        })
    }
}

pub async fn execute(command: &Command, deps: &Deps) -> Result<Answered, Refusal> {
    let ClientControlFrame::StatsDashboardHttp {
        resource,
        host,
        port,
        method,
        path,
        headers,
        body_base64,
        stream,
        ..
    } = &command.frame
    else {
        return Err(refused("the frame is not a dashboard request"));
    };
    let response = deps
        .dashboard
        .clone()
        .fetch(
            resource,
            host,
            *port,
            HttpRequest {
                method: method.clone(),
                path: path.clone(),
                headers: headers.clone(),
                body_base64: body_base64.clone(),
                stream: stream.clone(),
            },
        )
        .await?;
    let data = serde_json::to_value(response)
        .map_err(|_| refused("the dashboard response could not be encoded"))?;
    Ok(Answered::Reply(Reply::ok(&command.request_id, data)))
}

fn refused(message: &'static str) -> Refusal {
    Refusal::failed("stats-dashboard-http", message)
}

fn unavailable(context: &'static str, error: &reqwest::Error) -> Refusal {
    tracing::debug!(%error, context, "native dashboard request failed");
    Refusal::failed("stats-dashboard-http", context)
}

fn header_str<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn response_is_redirect(status: reqwest::StatusCode) -> bool {
    status.is_redirection() && status != reqwest::StatusCode::NOT_MODIFIED
}

fn decode_body(encoded: &str, limit: usize) -> Result<Vec<u8>, Refusal> {
    let body = STANDARD
        .decode(encoded)
        .map_err(|_| refused("the dashboard request body is not base64"))?;
    if body.len() > limit {
        return Err(refused("the dashboard request body exceeds its bound"));
    }
    Ok(body)
}

async fn read_bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, Refusal> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| unavailable("the dashboard response body failed", &error))?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(refused("the dashboard response body exceeds its bound"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_redirect_is_never_followed() {
        assert!(response_is_redirect(reqwest::StatusCode::FOUND));
        assert!(response_is_redirect(
            reqwest::StatusCode::TEMPORARY_REDIRECT
        ));
        assert!(!response_is_redirect(reqwest::StatusCode::NOT_MODIFIED));
        assert!(!response_is_redirect(reqwest::StatusCode::OK));
    }
    #[test]
    fn an_oversized_body_is_refused_before_it_is_read() {
        assert!(
            decode_body(
                &STANDARD.encode(vec![0_u8; MAX_REQUEST_BODY_BYTES + 1]),
                MAX_REQUEST_BODY_BYTES
            )
            .is_err()
        );
    }
}
