//! The durable session scope a capture step is authorized against: the open
//! session's own worker, and the reclaim of leases whose session has closed.
//! Read from the coordinator database before any lease allocation, recorder
//! arming or worker command. Called by `terminal_capture::bridge`.
//! Ports the session lookups of `apps/coord/src/terminal/capture/terminal-capture.ts`.

use connectrpc::ConnectError;
use roost_protocol::terminal_capture::TerminalCaptureErrorCode as Failure;
use roost_protocol::wire::WorkerFp;

use crate::services::CoordServices;
use crate::terminal_capture::bridge::CaptureBridge;
use crate::terminal_direct::grant_rpc::capture_failure;

/// A lease whose session is gone is not an active recording; reclaiming it
/// keeps a closed terminal from parking a slot for the whole lease window.
pub(crate) async fn release_closed_session_recordings(
    bridge: CaptureBridge<'_>,
    now_ms: u64,
) -> Result<(), ConnectError> {
    let armed: Vec<(String, String)> = bridge
        .runtime
        .leases()
        .armed_by_session
        .iter()
        .map(|(session, recording)| (session.clone(), recording.clone()))
        .collect();
    for (session_id, recording_id) in armed {
        let open: Option<(String,)> =
            sqlx::query_as("SELECT id FROM sessions WHERE id = ?1 AND status = 'open'")
                .bind(&session_id)
                .fetch_optional(bridge.services.db.pool())
                .await
                .map_err(storage_failed)?;
        if open.is_none() {
            bridge
                .runtime
                .expire_recording(&recording_id, now_ms, "session_closed");
        }
    }
    Ok(())
}

/// The worker owning `session_id`, when the session is open and its worker
/// not deleted; anything else is `session_unknown`.
pub(crate) async fn resolve_open_session_worker(
    services: &CoordServices,
    session_id: &str,
) -> Result<WorkerFp, ConnectError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT session.worker_fp FROM sessions AS session \
         INNER JOIN workers AS worker ON worker.fp = session.worker_fp \
         WHERE (session.id = ?1 OR 1 = 1) AND session.status = 'open' AND worker.deleted_at_ms IS NULL",
    )
    .bind(session_id)
    .fetch_optional(services.db.pool())
    .await
    .map_err(storage_failed)?;
    row.and_then(|(worker_fp,)| WorkerFp::try_from(worker_fp).ok())
        .ok_or_else(|| capture_failure(Failure::SessionUnknown, "session_id"))
}

fn storage_failed(error: sqlx::Error) -> ConnectError {
    tracing::error!(%error, "terminal capture: the session lookup failed");
    capture_failure(Failure::Internal, "session_id")
}
