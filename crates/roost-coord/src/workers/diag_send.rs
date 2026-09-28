//! Bounded `diag-snapshot` requests over live worker links: one correlated
//! browser command per worker, settled only by that worker's `rpc-ok`, each
//! with its own deadline, and every failure reported as a named result.
//! Ports `apps/coord/src/diagnostics/worker-diag-snapshot.ts`
//! (`collectWorkerDiagSnapshots`). Called by `diagnostics::worker_results`;
//! correlates through `services.scrollback.pending()`.

use std::collections::{BTreeMap, BTreeSet};

use connectrpc::{ConnectError, ErrorCode};
use futures_util::future::join_all;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde_json::{Map, Value, json};
use tokio::time::{Duration, Instant};

use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::workers::send::{SendOutcome, SendRefusal, current_routable_worker, send_frame_through};

/// The wait a caller that names no timeout gets. v2 `DIAG_SNAPSHOT_TIMEOUT_MS`.
pub const DIAG_SNAPSHOT_TIMEOUT_MS: u64 = 2_000;

const DIAG_SNAPSHOT_MAX_TIMEOUT_MS: u64 = 10_000;
const DIAG_ERROR_MESSAGE_CHARS: usize = 240;

/// The browser and viewer id a coordinator-originated diagnostic carries.
pub const COORD_DIAG_BROWSER_ID: &str = "coordinator-diag";

/// Why one worker produced no snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerDiagSnapshotErrorCode {
    /// No routable generation, or the link went away before the reply.
    Offline,
    /// The worker did not reply inside the bounded wait.
    Timeout,
    /// The socket refused the request frame.
    SendFailed,
    /// The worker answered with something that is not a snapshot.
    RpcError,
}

impl WorkerDiagSnapshotErrorCode {
    /// The wire spelling a diagnostic document carries.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Timeout => "timeout",
            Self::SendFailed => "send_failed",
            Self::RpcError => "rpc_error",
        }
    }
}

/// One worker's answer.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerDiagSnapshotResult {
    /// The worker's snapshot object, as it sent it.
    Ok {
        response_ms: u64,
        snapshot: Map<String, Value>,
    },
    /// No snapshot; `message` is clipped to 240 characters.
    Error {
        response_ms: u64,
        code: WorkerDiagSnapshotErrorCode,
        message: String,
    },
}

impl WorkerDiagSnapshotResult {
    fn error(started_at: Instant, code: WorkerDiagSnapshotErrorCode, message: &str) -> Self {
        Self::Error {
            response_ms: elapsed_ms(started_at),
            code,
            message: message.chars().take(DIAG_ERROR_MESSAGE_CHARS).collect(),
        }
    }

    /// The JSON envelope v2 puts under `workers[fp]`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::Ok {
                response_ms,
                snapshot,
            } => json!({ "status": "ok", "response_ms": response_ms, "snapshot": snapshot }),
            Self::Error {
                response_ms,
                code,
                message,
            } => json!({
                "status": "error",
                "response_ms": response_ms,
                "error": { "code": code.as_str(), "message": message },
            }),
        }
    }
}

/// The wait a fan-out may spend: the caller's, clamped to 1..=10000 ms.
#[must_use]
pub fn bounded_diag_snapshot_timeout_ms(timeout_ms: u64) -> u64 {
    timeout_ms.clamp(1, DIAG_SNAPSHOT_MAX_TIMEOUT_MS)
}

/// One bounded, correlated request per distinct worker, concurrently, keyed by
/// the authenticated fingerprint (never by anything in the payload). Each
/// request owns its pending entry, so a failure in one cannot touch another.
pub async fn collect_worker_diag_snapshots(
    relay: &ScrollbackRelay,
    worker_fps: impl IntoIterator<Item = WorkerFp>,
    timeout_ms: u64,
) -> BTreeMap<WorkerFp, WorkerDiagSnapshotResult> {
    let bounded = bounded_diag_snapshot_timeout_ms(timeout_ms);
    let fingerprints: BTreeSet<WorkerFp> = worker_fps.into_iter().collect();
    let results = join_all(
        fingerprints
            .iter()
            .map(|worker_fp| request_worker_diag_snapshot(relay, worker_fp, bounded)),
    )
    .await;
    fingerprints.into_iter().zip(results).collect()
}

async fn request_worker_diag_snapshot(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    timeout_ms: u64,
) -> WorkerDiagSnapshotResult {
    let started_at = Instant::now();
    let failed = |code: WorkerDiagSnapshotErrorCode, message: &str| {
        tracing::warn!(worker_fp = %worker_fp, code = code.as_str(), message, "worker diag snapshot failed");
        WorkerDiagSnapshotResult::error(started_at, code, message)
    };
    let Some(worker) = current_routable_worker(relay.workers(), worker_fp) else {
        return failed(
            WorkerDiagSnapshotErrorCode::Offline,
            "worker is not connected",
        );
    };
    let mut pending = match relay
        .pending()
        .create_fresh(Some(worker_fp.as_str()), relay.now_ms())
    {
        Ok(pending) => pending,
        Err(error) => {
            return failed(WorkerDiagSnapshotErrorCode::RpcError, &message_of(&error));
        }
    };
    let request_id = pending.request_id().to_owned();
    let frame = CoordWorkerDownstream::BrowserCommand {
        browser_id: COORD_DIAG_BROWSER_ID.to_owned(),
        viewer_id: COORD_DIAG_BROWSER_ID.to_owned(),
        request_id: request_id.clone(),
        frame: ClientControlFrame::DiagSnapshot {
            request_id: request_id.clone(),
            trace_id: None,
        },
        trace_id: None,
    };
    // A refused write drops `pending` at return, so a late reply finds nothing.
    match send_frame_through(relay.workers(), &worker, frame) {
        SendOutcome::Admitted { .. } => {}
        SendOutcome::Refused(SendRefusal::TransportDropped { .. }) => {
            return failed(
                WorkerDiagSnapshotErrorCode::SendFailed,
                "worker transport dropped request",
            );
        }
        SendOutcome::Refused(_) => {
            return failed(
                WorkerDiagSnapshotErrorCode::Offline,
                "worker is not connected",
            );
        }
    }
    let wait_until = started_at + Duration::from_millis(timeout_ms);
    let settled = match tokio::time::timeout_at(wait_until, pending.settle()).await {
        Ok(settled) => settled,
        Err(_) => Err(ConnectError::new(
            ErrorCode::DeadlineExceeded,
            format!("worker did not reply within {timeout_ms}ms"),
        )),
    };
    match settled {
        Ok(Value::Object(snapshot)) => WorkerDiagSnapshotResult::Ok {
            response_ms: elapsed_ms(started_at),
            snapshot,
        },
        Ok(_) => failed(
            WorkerDiagSnapshotErrorCode::RpcError,
            "worker returned an invalid diagnostic snapshot",
        ),
        Err(error) => failed(code_of(&error), &message_of(&error)),
    }
}

fn code_of(error: &ConnectError) -> WorkerDiagSnapshotErrorCode {
    match error.code {
        ErrorCode::DeadlineExceeded => WorkerDiagSnapshotErrorCode::Timeout,
        ErrorCode::Unavailable => WorkerDiagSnapshotErrorCode::Offline,
        _ => WorkerDiagSnapshotErrorCode::RpcError,
    }
}

fn message_of(error: &ConnectError) -> String {
    error
        .message
        .clone()
        .unwrap_or_else(|| error.code.as_str().to_owned())
}

fn elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}
