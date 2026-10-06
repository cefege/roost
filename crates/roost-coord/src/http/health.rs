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
