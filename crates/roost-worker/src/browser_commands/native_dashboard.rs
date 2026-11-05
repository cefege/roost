//! The worker end of a hosted stats-dashboard capability.
//!
//! The coordinator has already admitted the native host service that named
//! this origin. Before dialling, the worker proves the reported host resolves
//! to an address this machine actually owns (an OS bind on an ephemeral port),
//! pins that IP so DNS cannot rebind it, then proves the answer identifies as
//! OMP's dashboard and bounds everything that crosses the link. Redirects are refused rather than followed, credentials are dropped
//! before the request, and `Set-Cookie`/`Location` never leave the worker.
//!
//! A target is verified once per short window with a safe `GET /`, then each
//! real response must still carry the dashboard identity headers. The first
//! request for a target also opens one `/api/events` connection and drops it,
//! which is what starts the native background ingest the page expects.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use roost_protocol::native_dashboard::{
    HttpHeader, HttpRequest, HttpResponse, MAX_REQUEST_BODY_BYTES, MAX_RESPONSE_BODY_BYTES,
    dashboard_host_is_valid, is_dashboard_identity, method_is_allowed, path_is_safe,
    sanitize_response_headers,
};
use roost_protocol::wire::control::ClientControlFrame;

use super::{Answered, Boxed, Command, Deps, Refusal, Reply};

mod target;
use target::{PinnedTarget, ip_is_locally_assigned, resolve_local_target};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const VERIFIED_FOR: Duration = Duration::from_secs(30);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
/// A pinned target is never re-resolved, so DNS cannot rebind it later.
const MAX_PINNED_TARGETS: usize = 16;
const MAX_VERIFIED_TARGETS: usize = 16;

/// One upstream request, as the worker's capability sees it.
pub trait NativeDashboardProxy: Send + Sync {
    fn fetch(
        self: Arc<Self>,
        host: &str,
        port: u16,
        request: HttpRequest,
    ) -> Boxed<Result<HttpResponse, Refusal>>;
}

/// The production proxy: local HTTP with no proxy, no redirects, pinned
/// addresses, and a bounded body in both directions.
pub struct NativeDashboardHttp {
    client: reqwest::Client,
    pinned: Mutex<HashMap<String, PinnedTarget>>,
    verified: Mutex<HashMap<String, Instant>>,
    live_started: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for NativeDashboardHttp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeDashboardHttp")
            .finish_non_exhaustive()
    }
}

impl NativeDashboardHttp {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "stats dashboard HTTP client fell back to defaults");
                reqwest::Client::new()
            });
        Self {
            client,
            pinned: Mutex::new(HashMap::new()),
            verified: Mutex::new(HashMap::new()),
            live_started: Mutex::new(HashSet::new()),
        }
    }

    async fn pin_target(&self, host: &str, port: u16) -> Result<PinnedTarget, Refusal> {
        if port == 0 || !dashboard_host_is_valid(host) {
            return Err(refused("the dashboard target is not a valid local address"));
        }
        let key = format!("{host}:{port}");
        {
            let pinned = self
                .pinned
                .lock()
                .map_err(|_| refused("the dashboard pin table is poisoned"))?;
            if let Some(target) = pinned.get(&key) {
                return Ok(target.clone());
            }
        }
        let host_owned = host.to_owned();
        let resolved = tokio::time::timeout(
            RESOLVE_TIMEOUT,
            tokio::task::spawn_blocking(move || resolve_local_target(&host_owned, port)),
        )
        .await
        .map_err(|_| refused("resolving the dashboard address timed out"))?
        .map_err(|_| refused("resolving the dashboard address failed"))?
        .ok_or_else(|| refused("the dashboard host is not assigned to this machine"))?;
        let mut pinned = self
            .pinned
            .lock()
            .map_err(|_| refused("the dashboard pin table is poisoned"))?;
        if pinned.len() >= MAX_PINNED_TARGETS && !pinned.contains_key(&key) {
            // The bound is a memory guarantee, not a trust cache: a cleared
            // entry is re-resolved and re-proved against the local bind before
            // its next request, so dropping it cannot widen the destination.
            pinned.clear();
        }
        pinned.insert(key, resolved.clone());
        Ok(resolved)
    }

    async fn verify(&self, target: &PinnedTarget) -> Result<(), Refusal> {
        let authority = target.authority();
        {
            let verified = self
                .verified
                .lock()
                .map_err(|_| refused("the dashboard verification table is poisoned"))?;
            if verified
                .get(&authority)
                .is_some_and(|at| at.elapsed() < VERIFIED_FOR)
            {
                return Ok(());
            }
        }
        let response = self
            .client
            .get(format!("http://{authority}/"))
            .timeout(PROBE_TIMEOUT)
            .send()
            .await
            .map_err(|error| unavailable("the native dashboard did not answer", &error))?;
        let version = header_str(response.headers(), "x-omp-stats-dashboard");
        let hostname = header_str(response.headers(), "x-omp-stats-hostname");
        if !response.status().is_success()
            || !is_dashboard_identity(version, hostname, &target.reported_host)
        {
            return Err(refused(
                "the target does not identify as the OMP stats dashboard",
            ));
        }
        drop(response);
        let mut verified = self
            .verified
            .lock()
            .map_err(|_| refused("the dashboard verification table is poisoned"))?;
        if verified.len() >= MAX_VERIFIED_TARGETS {
            verified.clear();
        }
        verified.insert(authority, Instant::now());
        Ok(())
    }

    fn start_live_once(&self, target: &PinnedTarget) {
        let authority = target.authority();
        let Ok(mut started) = self.live_started.lock() else {
            return;
        };
        if started.len() >= MAX_VERIFIED_TARGETS {
            started.clear();
        }
        if !started.insert(authority.clone()) {
            return;
        }
        let client = self.client.clone();
        // Opening and dropping the stream is what `StatsLive.start` runs for;
        // it unwatches nobody, because the page's own EventSource is served by
        // the coordinator's status bridge.
        tokio::spawn(async move {
            if let Ok(response) = client
                .get(format!("http://{authority}/api/events"))
                .header("accept", "text/event-stream")
                .timeout(PROBE_TIMEOUT)
                .send()
                .await
            {
                drop(response);
            }
        });
    }
}

impl Default for NativeDashboardHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeDashboardProxy for NativeDashboardHttp {
    fn fetch(
        self: Arc<Self>,
        host: &str,
        port: u16,
        request: HttpRequest,
    ) -> Boxed<Result<HttpResponse, Refusal>> {
        let host = host.to_owned();
        Box::pin(async move {
            if !method_is_allowed(&request.method) || !path_is_safe(&request.path) {
                return Err(refused(
                    "the dashboard request is outside the bridge contract",
                ));
            }
            let body = decode_body(&request.body_base64, MAX_REQUEST_BODY_BYTES)?;
            let target = self.pin_target(&host, port).await?;
            // A cached pin is re-proved before every request: an address can
            // cease to be assigned (VPN down, interface removed), and a stale
            // pin must not keep dialling it.
            if !ip_is_locally_assigned(target.dial) {
                if let Ok(mut pins) = self.pinned.lock() {
                    pins.retain(|_, pinned| pinned != &target);
                }
                return Err(refused(
                    "the dashboard address is no longer assigned to this machine",
                ));
            }
            self.verify(&target).await?;
            self.start_live_once(&target);

            let method = reqwest::Method::from_bytes(request.method.as_bytes())
                .map_err(|_| refused("the dashboard method is not an HTTP method"))?;
            let mut builder = self
                .client
                .request(
                    method,
                    format!("http://{}{}", target.authority(), request.path),
                )
                .timeout(REQUEST_TIMEOUT);
            for header in
                roost_protocol::native_dashboard::sanitize_request_headers(request.headers)
            {
                builder = builder.header(header.name, header.value);
            }
            if !body.is_empty() {
                builder = builder.body(body);
            }
            let response = builder
                .send()
                .await
                .map_err(|error| unavailable("the native dashboard request failed", &error))?;
            if response_is_redirect(response.status()) {
                return Err(refused("the native dashboard redirected an HTTP request"));
            }
            let version = header_str(response.headers(), "x-omp-stats-dashboard");
            let hostname = header_str(response.headers(), "x-omp-stats-hostname");
            if !is_dashboard_identity(version, hostname, &target.reported_host) {
                return Err(refused(
                    "the target stopped identifying as the OMP stats dashboard",
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
            let body = read_bounded(response, MAX_RESPONSE_BODY_BYTES).await?;
            Ok(HttpResponse {
                status,
                headers,
                body_base64: if body.is_empty() {
                    String::new()
                } else {
                    STANDARD.encode(body)
                },
            })
        })
    }
}

/// Run one dashboard frame. The frame is the only producer of this upstream
/// request, so a shape mismatch is an internal defect, not caller input.
pub async fn execute(command: &Command, deps: &Deps) -> Result<Answered, Refusal> {
    let ClientControlFrame::StatsDashboardHttp {
        host,
        port,
        method,
        path,
        headers,
        body_base64,
        ..
    } = &command.frame
    else {
        return Err(Refusal::failed(
            "stats-dashboard-http",
            "the frame is not a dashboard request",
        ));
    };
    let response = deps
        .dashboard
        .clone()
        .fetch(
            host,
            *port,
            HttpRequest {
                method: method.clone(),
                path: path.clone(),
                headers: headers.clone(),
                body_base64: body_base64.clone(),
            },
        )
        .await?;
    let data = serde_json::to_value(response).map_err(|_| {
        Refusal::failed(
            "stats-dashboard-http",
            "the dashboard response could not be encoded",
        )
    })?;
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
    // 304 is a successful revalidation for the trace ETag endpoints and carries
    // no Location. Every other 3xx is a navigation escape and is refused.
    status.is_redirection() && status != reqwest::StatusCode::NOT_MODIFIED
}

fn decode_body(encoded: &str, limit: usize) -> Result<Vec<u8>, Refusal> {
    if encoded.is_empty() {
        return Ok(Vec::new());
    }
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
        let oversized = STANDARD.encode(vec![0_u8; MAX_REQUEST_BODY_BYTES + 1]);
        assert!(decode_body(&oversized, MAX_REQUEST_BODY_BYTES).is_err());
    }
}
