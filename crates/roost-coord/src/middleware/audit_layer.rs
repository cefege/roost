//! The per-request audit mount: one [`AuditRecord`] for every response the
//! listener answers itself, and none for the ones another layer already owns.
//!
//! Owned by the middleware stack; `http::listener` mounts [`audit_layer`] below
//! the CORS layer and above the routes. It is a MOUNT of
//! [`crate::middleware::audit`], which owns every decision about whether a row
//! is worth writing; this module only knows which surface answered and hands the
//! hook one record.
//!
//! EXACTLY ONE RECORD PER REQUEST, AND THE SPLIT IS BY SURFACE. A Connect RPC's
//! row is written by the auth interceptor, which is the only layer that
//! resolved the credential, so this layer writes nothing for a path under
//! `/roost.v1.CoordinatorService/`. That is a better partition than relying on
//! the hook's `AlreadyRecorded` guard: a guard that fires on every Connect
//! request is a guard that fires all the time, and teaches its reader to ignore
//! it.
//!
//! THE UPGRADES AND THE RETIRED `Sync` ARE ANSWERED ABOVE THIS BLOCK IN v2
//! (`bun-coordinator-listeners.ts:324-340`), so they get no row here either. A
//! WebSocket upgrade is not a request/response pair at all -- its lifetime is
//! the socket, and the refusal that matters is the one the upgrade's own
//! admission module logs with the caller's fingerprint.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::coord_core::CoordCore;
use crate::http::listener::{CONNECT_PATH_PREFIX, DB_EXPORT_PATH};
use crate::middleware::audit::{
    AuditOutcome, AuditRecord, NonConnectSurface, record_request,
    should_persist_non_connect_audit,
};
use crate::sync_ws::upgrade_admission::SYNC_WS_PATH;
use crate::worker_link::upgrade_admission::WORKER_WS_PATH_PREFIX;

/// The log target this mount reports under, the same one the hook uses.
const AUDIT_TARGET: &str = "middleware.audit";

/// The client log-correlation id, when the caller sent one.
///
/// Read rather than minted: the id that correlates this row with the browser's
/// own log line is the one the browser already has, and a second id would
/// correlate nothing.
const TRACE_ID_HEADER: &str = "x-roost-trace-id";

/// The API namespace, which no route owns outside the export.
const API_PREFIX: &str = "/api/";

/// The proto service namespace, which Connect owns.
const PROTO_PREFIX: &str = "/roost.";

/// What the audit mount needs from the listener it is mounted in.
#[derive(Debug, Clone)]
pub struct AuditMount {
    /// The shared process state the hook writes through.
    core: Arc<CoordCore>,
    /// Whether an SPA build is available to serve.
    ///
    /// It decides the surface of an unmatched page path, and therefore whether
    /// the row would be kept: a served page read is skipped, and a path nothing
    /// claimed is skipped as a 404 probe.
    spa_available: bool,
}

impl AuditMount {
    /// The mount for a listener with this process state and this build.
    #[must_use]
    pub fn new(core: Arc<CoordCore>, spa_available: bool) -> Self {
        Self { core, spa_available }
    }
}

/// The non-Connect surface that answers this request, or `None` when the
/// request is not this layer's to record.
fn surface_of(method: &str, path: &str, spa_available: bool) -> Option<NonConnectSurface> {
    if path.starts_with(CONNECT_PATH_PREFIX)
        || path.starts_with(WORKER_WS_PATH_PREFIX)
        || path == SYNC_WS_PATH
    {
        return None;
    }
    if path == DB_EXPORT_PATH {
        return Some(NonConnectSurface::DbExport);
    }
    // A `POST` to the proto namespace that Connect did not claim is an API
    // miss, not a page (`coord-factory.ts:174-176`).
    if path.starts_with(API_PREFIX) || (method == "POST" && path.starts_with(PROTO_PREFIX)) {
        return Some(NonConnectSurface::Api);
    }
    Some(if spa_available {
        NonConnectSurface::Spa
    } else {
        NonConnectSurface::Api
    })
}

/// The `x-roost-trace-id` this request carries, if any.
fn trace_id(request: &Request) -> Option<String> {
    request
        .headers()
        .get(TRACE_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Write this request's row, if the surface and the outcome are worth one.
pub async fn audit_layer(
    State(mount): State<AuditMount>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let Some(surface) = surface_of(&method, &path, mount.spa_available) else {
        return next.run(request).await;
    };
    let trace = trace_id(&request);
    let response = next.run(request).await;
    let status = response.status().as_u16();
    if !should_persist_non_connect_audit(surface, &method, status) {
        return response;
    }
    let mut record = AuditRecord::non_connect(surface, &method, &path, status, trace);
    // The hook cannot fail, and its outcome is never propagated into the
    // response: an audit outage that became an availability outage would have
    // swapped one bad day for another. A write failure is a fact about the
    // audit store, and the hook has already logged it.
    let outcome = record_request(&mount.core, &mut record).await;
    tracing::debug!(
        target: AUDIT_TARGET,
        ?outcome,
        path,
        status,
        "non-Connect request audited"
    );
    response
}
