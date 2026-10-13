//! The liveness and readiness probes an orchestrator polls: `/healthz` answers
//! while the process serves at all, `/readyz` only while it should be sent
//! traffic — its database answers and it is not draining.
//!
//! Mounted by `http::listener::build_router` as plain routes; the audit layer
//! skips both paths and the rate limiter only meters Connect methods, so a probe
//! every few seconds writes no row and spends no budget. `serve` raises the
//! draining flag on shutdown.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::http::listener::ListenerState;

/// The liveness probe path.
pub const HEALTHZ_PATH: &str = "/healthz";

/// The readiness probe path.
pub const READYZ_PATH: &str = "/readyz";

/// How long the readiness probe waits on the database. Under an orchestrator's
/// own probe timeout, so a hung database answers 503 rather than a timeout the
/// kubelet cannot tell from a hung process.
pub const READINESS_DATABASE_TIMEOUT: Duration = Duration::from_secs(2);

/// The two probe routes, for the listener to merge.
pub fn health_routes() -> Router<Arc<ListenerState>> {
    Router::new()
        .route(HEALTHZ_PATH, get(healthz))
        .route(READYZ_PATH, get(readyz))
}

/// State for the optional per-replica probe listener.
#[derive(Debug, Clone)]
pub struct ProbeState {
    /// The database and notifier owned by this coordinator process.
    pub services: Arc<crate::services::CoordServices>,
}

/// Routes that intentionally expose no RPC, SPA or WebSocket surface.
pub fn probe_routes() -> Router<Arc<ProbeState>> {
    Router::new()
        .route(HEALTHZ_PATH, get(probe_healthz))
        .route(READYZ_PATH, get(probe_readyz))
}

async fn probe_healthz() -> Response {
    plain(StatusCode::OK, "ok")
}

async fn probe_readyz(State(state): State<Arc<ProbeState>>) -> Response {
    let ownership = &state.services.ownership;
    let now_ms = crate::serve::now_ms();
    if !state.services.cluster.connected()
        || !ownership.heartbeat_fresh(now_ms)
        || (!ownership.is_owner() && ownership.owner_internal_url().is_none())
    {
        return plain(StatusCode::SERVICE_UNAVAILABLE, "cluster unavailable");
    }
    let probe = sqlx::query("SELECT 1").execute(state.services.db.pool());
    match tokio::time::timeout(READINESS_DATABASE_TIMEOUT, probe).await {
        Ok(Ok(_)) => plain(StatusCode::OK, "ready"),
        Ok(Err(error)) => {
            tracing::warn!(%error, "replica readiness probe: database check failed");
            plain(StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
        }
        Err(_) => {
            tracing::warn!(
                timeout_ms = READINESS_DATABASE_TIMEOUT.as_millis(),
                "replica readiness probe: database check timed out"
            );
            plain(StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
        }
    }
}

async fn healthz() -> Response {
    plain(StatusCode::OK, "ok")
}

async fn readyz(State(state): State<Arc<ListenerState>>) -> Response {
    if state.draining.load(Ordering::Acquire) {
        return plain(StatusCode::SERVICE_UNAVAILABLE, "draining");
    }
    let probe = sqlx::query("SELECT 1").execute(state.services.db.pool());
    match tokio::time::timeout(READINESS_DATABASE_TIMEOUT, probe).await {
        Ok(Ok(_)) => plain(StatusCode::OK, "ready"),
        Ok(Err(error)) => {
            tracing::warn!(%error, "readiness probe: database check failed");
            plain(StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
        }
        Err(_) => {
            tracing::warn!(
                timeout_ms = READINESS_DATABASE_TIMEOUT.as_millis(),
                "readiness probe: database check timed out"
            );
            plain(StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
        }
    }
}

fn plain(status: StatusCode, body: &'static str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}
