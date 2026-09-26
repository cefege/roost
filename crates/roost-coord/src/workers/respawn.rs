//! Respawn-if-missing dispatch: after a worker's hello grace period, every open
//! session it still claims is offered back at the size its viewers already show.
//!
//! Ported from `apps/coord/src/workers/worker-respawn.ts`.
//!
//! WHY THE VIEW HUB IS ASKED FOR GEOMETRY AND NOTHING ELSE. v2 reads
//! `terminalViewSnapshot(row.id)?.effective` and discards the other four fields
//! (`worker-respawn.ts:67-68`), so this asks
//! `TerminalViewLifecycle::effective_geometry` directly: a snapshot that also
//! carried active views, parked views, a stream id and an unavailable flag would
//! be a second answer to "what size is this session" that a view-hub change
//! could drift from.
//!
//! NOTHING IS WATCHING IS NOT AN ERROR. With no viewer the PTY is framed at the
//! conventional default and the first view to attach reframes it; respawning at
//! a size while views DO exist makes every attached TUI redraw twice
//! (`worker-respawn.ts:22-24`).

use std::str::FromStr;
use std::sync::Arc;

use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use roost_protocol::wire::{SessionId, SessionKind};
use sqlx::{AssertSqlSafe, FromRow};

use crate::coord_core::seams::TerminalViewLifecycle;
use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::db::CoordDb;
use crate::write_gate::WriteGate;

use super::send::{SendOutcome, send_frame_through};

/// The width a respawn uses when nothing is watching.
pub const RESPAWN_UNWATCHED_COLS: i64 = 80;

/// The height a respawn uses when nothing is watching.
pub const RESPAWN_UNWATCHED_ROWS: i64 = 24;

/// The `browser_id` a coordinator-initiated respawn carries.
const COORD_BROWSER_ID: &str = "coord";

/// The `viewer_id` a coordinator-initiated respawn carries, so a worker's
/// presence can tell an operator-driven frame from a browser's.
const COORD_RESPAWN_VIEWER_ID: &str = "coord:respawn";

/// One open session a worker should be asked to revive.
#[derive(Debug, Clone, FromRow)]
struct OpenSessionRow {
    id: String,
    kind: String,
    cwd: String,
}

/// What one respawn pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RespawnReport {
    /// How many sessions were offered to the worker.
    pub dispatched: usize,
    /// How many rows were skipped: an unknown kind, an unaddressable id, or a
    /// frame the socket refused. A skip is logged, never guessed at.
    pub skipped: usize,
    /// Whether the pass was refused before it read anything, because an
    /// exclusive keeper-update drain holds the write gate.
    pub deferred: bool,
}

/// Offer every open session of this worker back to it, through `handle`.
///
/// The write-gate lease is not optional: a reconnect must not recreate a channel
/// while the coordinator is proving the keeper empty for an update.
pub async fn respawn_missing_for_worker(
    database: &CoordDb,
    registry: &WorkerRegistry,
    views: &dyn TerminalViewLifecycle,
    gate: &WriteGate,
    handle: &Arc<WorkerHandle>,
) -> RespawnReport {
    let Ok(lease) = gate.acquire_shared() else {
        tracing::warn!(
            worker_fp = %handle.worker_fp,
            "respawn deferred: a keeper update holds the write gate"
        );
        return RespawnReport {
            deferred: true,
            ..RespawnReport::default()
        };
    };
    let report = dispatch(database, registry, views, handle).await;
    drop(lease);
    report
}

/// The pass itself, with the gate already held.
async fn dispatch(
    database: &CoordDb,
    registry: &WorkerRegistry,
    views: &dyn TerminalViewLifecycle,
    handle: &Arc<WorkerHandle>,
) -> RespawnReport {
    let mut report = RespawnReport::default();
    // Checked before the read and again before every send: a handle that stopped
    // being current while the query was in flight must not be written to.
    if !handle.is_routable() {
        return report;
    }
    let rows = match read_open_sessions(database, handle.worker_fp.as_str()).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(
                worker_fp = %handle.worker_fp,
                %error,
                "respawn session read failed"
            );
            return report;
        }
    };
    for row in rows {
        if !handle.is_routable() {
            return report;
        }
        let Some(session_id) = addressable(&row.id, handle.worker_fp.as_str()) else {
            report.skipped += 1;
            continue;
        };
        if !known_kind(&row.kind, handle.worker_fp.as_str(), &row.id) {
            report.skipped += 1;
            continue;
        }
        let geometry = views.effective_geometry(&session_id);
        let cols = geometry.map_or(RESPAWN_UNWATCHED_COLS, |geometry| i64::from(geometry.cols));
        let height = geometry.map_or(RESPAWN_UNWATCHED_ROWS, |geometry| i64::from(geometry.rows));
        let request_id = format!("coord:respawn:{session_id}");
        let outcome = send_frame_through(
            registry,
            handle,
            CoordWorkerDownstream::BrowserCommand {
                browser_id: COORD_BROWSER_ID.to_owned(),
                viewer_id: COORD_RESPAWN_VIEWER_ID.to_owned(),
                request_id: request_id.clone(),
                frame: ClientControlFrame::RespawnIfMissing {
                    request_id,
                    session_id: session_id.clone(),
                    cwd: row.cwd.clone(),
                    cols,
                    rows: height,
                    trace_id: None,
                },
                trace_id: None,
            },
        );
        match outcome {
            SendOutcome::Admitted { .. } => {
                tracing::info!(
                    worker_fp = %handle.worker_fp,
                    %session_id,
                    cols,
                    rows = height,
                    watched = geometry.is_some(),
                    "respawn_missing_dispatch"
                );
                report.dispatched += 1;
            }
            SendOutcome::Refused(refusal) => {
                tracing::warn!(
                    worker_fp = %handle.worker_fp,
                    %session_id,
                    %refusal,
                    "respawn send failed"
                );
                report.skipped += 1;
            }
        }
    }
    report
}

/// A session id a worker can be asked about, or `None` with the reason logged.
fn addressable(session_id: &str, worker_fp: &str) -> Option<SessionId> {
    match SessionId::try_from(session_id) {
        Ok(session_id) => Some(session_id),
        Err(_) => {
            tracing::warn!(
                %worker_fp,
                session_id,
                "respawn skipped a session whose id is not addressable"
            );
            None
        }
    }
}

/// Whether the stored kind is one this build knows how to revive.
fn known_kind(kind: &str, worker_fp: &str, session_id: &str) -> bool {
    if SessionKind::from_str(kind).is_ok() {
        return true;
    }
    tracing::warn!(
        %worker_fp,
        %session_id,
        kind,
        "respawn skipped a session of an unknown kind"
    );
    false
}

/// The open sessions a live worker still claims.
///
/// The join on `workers.deleted_at_ms IS NULL` is not decoration: a deleted
/// worker's rows are tombstoned, and reviving a PTY on a machine an operator
/// removed is the one outcome a delete exists to prevent.
async fn read_open_sessions(
    database: &CoordDb,
    worker_fp: &str,
) -> Result<Vec<OpenSessionRow>, sqlx::Error> {
    sqlx::query_as::<_, OpenSessionRow>(AssertSqlSafe(
        "SELECT session.id AS id, session.kind AS kind, session.cwd AS cwd \
         FROM sessions AS session \
         INNER JOIN workers AS worker ON worker.fp = session.worker_fp \
         WHERE session.worker_fp = ? AND session.status = 'open' \
         AND worker.deleted_at_ms IS NULL \
         ORDER BY session.created_at, session.id",
    ))
    .bind(worker_fp)
    .fetch_all(database.pool())
    .await
}
