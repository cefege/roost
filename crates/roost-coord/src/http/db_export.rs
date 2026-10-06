//! The on-host database export: a consistent copy of the coordinator's SQLite
//! file, streamed to a caller that arrived on this host.
//!
//! Mounted by `http::listener` at `DB_EXPORT_PATH`; the copy is
//! `maintenance::export_snapshot`'s, and the on-host answer is the one the
//! caller-origin layer resolved. A Postgres-backed coordinator has no file to
//! export and answers 404.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};

use crate::http::listener::ListenerState;
use crate::maintenance::export_snapshot::{
    EXPORT_DOWNLOAD_NAME, ExportSnapshot, prepare_export_snapshot, schedule_reclaim,
};
use crate::middleware::caller_origin::{ON_HOST_ONLY, from_extensions};

/// The on-host database export.
pub async fn db_export(State(state): State<Arc<ListenerState>>, request: Request) -> Response {
    // The on-host gate is the whole authorization for this route: no rate limit
    // and no second token. `MiscDbExportUrl`, which is how a caller discovers
    // this path, requires a device principal AND on-host, so a caller that is
    // not on this host is refused here rather than answered.
    match from_extensions(request.extensions()) {
        Some(origin) if origin.on_host => {}
        Some(origin) => {
            tracing::warn!(
                client_ip = %origin.client_ip,
                listener = ?origin.listener,
                "db-export refused for a caller that did not arrive on this host"
            );
            return on_host_refusal();
        }
        None => {
            // Every request that reached a handler passed the caller-origin
            // layer, so an absent profile is a wiring fault. It fails closed:
            // this route's answer is a whole database.
            tracing::error!(
                "db-export has no resolved caller origin: the caller-origin layer is not mounted"
            );
            return on_host_refusal();
        }
    }
    // Only a SQLite file the coordinator owns can be exported; a Postgres
    // database answers 404 exactly as a missing file does.
    if !state
        .services
        .db
        .sqlite_path()
        .is_some_and(std::path::Path::exists)
    {
        return (axum::http::StatusCode::NOT_FOUND, "").into_response();
    }
    let snapshot = match prepare_export_snapshot(&state.services.db).await {
        Ok(snapshot) => snapshot,
        Err(reason) => {
            // The copy is what failed; the caller asked for a database and is
            // getting none. 500 rather than 503: nothing here is a retryable
            // outage, and a browser that retries a broken disk fills it faster.
            tracing::error!(%reason, "db-export: the snapshot could not be taken");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                r#"{"error":"the export snapshot could not be taken"}"#,
            )
                .into_response();
        }
    };
    schedule_reclaim(snapshot.path.clone());
    match stream_database(&snapshot).await {
        Ok(response) => response,
        Err(reason) => {
            tracing::error!(%reason, "db-export: the snapshot could not be opened for streaming");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                r#"{"error":"the export snapshot could not be read"}"#,
            )
                .into_response()
        }
    }
}

/// Stream the copy from disk, never through memory: a coordinator database is
/// measured in hundreds of megabytes, and reading one into the heap to send it
/// is how an export becomes the outage it was taken to diagnose.
async fn stream_database(snapshot: &ExportSnapshot) -> std::io::Result<Response> {
    let file = tokio::fs::File::open(&snapshot.path).await?;
    let mut headers = axum::http::HeaderMap::new();
    insert(
        &mut headers,
        axum::http::header::CONTENT_TYPE,
        "application/x-sqlite3",
    );
    insert(
        &mut headers,
        axum::http::header::CONTENT_LENGTH,
        &snapshot.size.to_string(),
    );
    insert(
        &mut headers,
        axum::http::header::CONTENT_DISPOSITION,
        &format!("attachment; filename=\"{EXPORT_DOWNLOAD_NAME}\""),
    );
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file));
    Ok((axum::http::StatusCode::OK, headers, body).into_response())
}

/// A header whose value could not be a header value is left out, not faked: an
/// export with a wrong `content-length` is worse than one without.
fn insert(headers: &mut axum::http::HeaderMap, name: axum::http::HeaderName, value: &str) {
    if let Ok(value) = axum::http::HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

/// The refusal the export route gives a caller that is not on this host.
fn on_host_refusal() -> Response {
    (
        axum::http::StatusCode::FORBIDDEN,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        format!(r#"{{"error":"{ON_HOST_ONLY}"}}"#),
    )
        .into_response()
}
