//! Typed, bounded terminal-pipeline evidence requests over routable worker
//! links: one `DTerminalPipelineSnapshotRequest` per worker, bound to the
//! generation current at send time, settled only by that worker's reply.
//! Ports the requester half of
//! `apps/coord/src/terminal/screen/worker-terminal-pipeline-snapshot.ts`.
//! Called by `pipeline_cache`; correlates through `services.scrollback.pending()`.

use std::collections::BTreeMap;

use connectrpc::{ConnectError, ErrorCode};
use futures_util::future::join_all;
use roost_proto::{
    DTerminalPipelineSnapshotRequest, TerminalPipelineTarget, WTerminalPipelineSnapshot,
};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use tokio::time::Instant;

use crate::terminal_screen::pipeline_snapshot::{
    TerminalPipelineDiagnosticTarget, normalize_terminal_pipeline_diagnostic_targets,
    terminal_pipeline_snapshot_matches_request,
};
use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::workers::hop_deadline::HopDeadline;
use crate::workers::send::{SendOutcome, SendRefusal, current_routable_worker, send_frame_through};
use crate::workers::terminal_request::TerminalWorkerRequest;

/// The wait a caller that names no timeout gets.
pub const TERMINAL_PIPELINE_DIAG_TIMEOUT_MS: u64 = 2_000;

const TERMINAL_PIPELINE_MAX_TIMEOUT_MS: u64 = 10_000;
const TERMINAL_PIPELINE_MAX_ERROR_MESSAGE_CHARS: usize = 240;

/// Why one worker's sample produced no evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalPipelineSnapshotErrorCode {
    /// No routable generation, a generation replaced mid-request, or a link
    /// that closed before the reply.
    Offline,
    /// The worker did not reply inside the bounded wait.
    Timeout,
    /// The socket refused the request frame.
    SendFailed,
    /// The worker replied with something that does not answer the request.
    RpcError,
}

impl TerminalPipelineSnapshotErrorCode {
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

/// One worker's answer to one bounded sample.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerTerminalPipelineSnapshotResult {
    /// A reply that matched the request exactly.
    Ok {
        response_ms: u64,
        snapshot: WTerminalPipelineSnapshot,
    },
    /// No evidence; `message` is clipped to 240 characters.
    Error {
        response_ms: u64,
        code: TerminalPipelineSnapshotErrorCode,
        message: String,
    },
}

impl WorkerTerminalPipelineSnapshotResult {
    /// An error result, with its message clipped to the diagnostic bound.
    #[must_use]
    pub fn error(response_ms: u64, code: TerminalPipelineSnapshotErrorCode, message: &str) -> Self {
        Self::Error {
            response_ms,
            code,
            message: message
                .chars()
                .take(TERMINAL_PIPELINE_MAX_ERROR_MESSAGE_CHARS)
                .collect(),
        }
    }
}

/// The wait a sample may spend: the caller's, clamped to 1..=10000 ms, or the
/// 2 s default when the caller names none.
#[must_use]
pub fn bounded_terminal_pipeline_timeout_ms(timeout_ms: Option<u64>) -> u64 {
    timeout_ms
        .unwrap_or(TERMINAL_PIPELINE_DIAG_TIMEOUT_MS)
        .clamp(1, TERMINAL_PIPELINE_MAX_TIMEOUT_MS)
}

/// Issues at most one bounded typed request per supplied worker, concurrently.
/// Target lists are normalized first; a worker whose list normalizes to
/// nothing is not asked and has no entry in the answer.
pub async fn collect_worker_terminal_pipeline_snapshots(
    relay: &ScrollbackRelay,
    targets_by_worker: &BTreeMap<WorkerFp, Vec<TerminalPipelineDiagnosticTarget>>,
    timeout_ms: Option<u64>,
) -> BTreeMap<WorkerFp, WorkerTerminalPipelineSnapshotResult> {
    let bounded_timeout_ms = bounded_terminal_pipeline_timeout_ms(timeout_ms);
    let requests: Vec<(&WorkerFp, Vec<TerminalPipelineDiagnosticTarget>)> = targets_by_worker
        .iter()
        .map(|(worker_fp, targets)| {
            (
                worker_fp,
                normalize_terminal_pipeline_diagnostic_targets(targets),
            )
        })
        .filter(|(_, targets)| !targets.is_empty())
        .collect();
    let results = join_all(requests.iter().map(|(worker_fp, targets)| {
        request_worker_terminal_pipeline_snapshot(relay, worker_fp, targets, bounded_timeout_ms)
    }))
    .await;
    requests
        .into_iter()
        .map(|(worker_fp, _)| worker_fp.clone())
        .zip(results)
        .collect()
}

/// Routes one session-scoped sample directly to the current authenticated
/// worker generation. The pending entry is keyed by the worker fingerprint, so
/// another worker's frame cannot settle it; a replaced generation is refused
/// by the send gate before anything is written.
async fn request_worker_terminal_pipeline_snapshot(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    targets: &[TerminalPipelineDiagnosticTarget],
    timeout_ms: u64,
) -> WorkerTerminalPipelineSnapshotResult {
    let started_at = Instant::now();
    let failed = |code: TerminalPipelineSnapshotErrorCode, message: &str| {
        tracing::warn!(worker_fp = %worker_fp, code = code.as_str(), message, "terminal pipeline sample failed");
        WorkerTerminalPipelineSnapshotResult::error(elapsed_ms(started_at), code, message)
    };
    let Some(worker) = current_routable_worker(relay.workers(), worker_fp) else {
        return failed(
            TerminalPipelineSnapshotErrorCode::Offline,
            "worker is not connected",
        );
    };
    let deadline = HopDeadline::start(timeout_ms);
    let pending = match relay
        .pending()
        .create_fresh(Some(worker_fp.as_str()), relay.now_ms())
    {
        Ok(pending) => pending,
        Err(error) => {
            return failed(
                TerminalPipelineSnapshotErrorCode::RpcError,
                &error_message(&error),
            );
        }
    };
    let request_id = pending.request_id().to_owned();
    let frame = CoordWorkerDownstream::TerminalPipelineSnapshot(DTerminalPipelineSnapshotRequest {
        request_id: request_id.clone(),
        targets: targets
            .iter()
            .map(|target| TerminalPipelineTarget {
                session_id: target.session_id.clone(),
                view_id: target.view_id.clone(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    });
    // A refused write leaves the table as soon as `pending` drops at return,
    // so a late reply under this id finds nothing to settle.
    match send_frame_through(relay.workers(), &worker, frame) {
        SendOutcome::Admitted { .. } => {}
        SendOutcome::Refused(SendRefusal::TransportDropped { .. }) => {
            return failed(
                TerminalPipelineSnapshotErrorCode::SendFailed,
                "worker transport dropped request",
            );
        }
        SendOutcome::Refused(_) => {
            return failed(
                TerminalPipelineSnapshotErrorCode::Offline,
                "worker connection changed before request",
            );
        }
    }
    tracing::debug!(
        worker_fp = %worker_fp,
        request_id,
        targets = targets.len(),
        timeout_ms,
        "terminal pipeline sample requested"
    );
    let request =
        TerminalWorkerRequest::<WTerminalPipelineSnapshot>::from_pending(pending, &deadline, true);
    match request.result().await {
        Ok(snapshot)
            if terminal_pipeline_snapshot_matches_request(&snapshot, &request_id, targets) =>
        {
            let response_ms = elapsed_ms(started_at);
            tracing::debug!(worker_fp = %worker_fp, request_id, response_ms, "terminal pipeline sample answered");
            WorkerTerminalPipelineSnapshotResult::Ok {
                response_ms,
                snapshot,
            }
        }
        Ok(_) => failed(
            TerminalPipelineSnapshotErrorCode::RpcError,
            "worker returned an invalid terminal pipeline snapshot",
        ),
        Err(error) => failed(error_code(&error), &error_message(&error)),
    }
}

fn error_code(error: &ConnectError) -> TerminalPipelineSnapshotErrorCode {
    match error.code {
        ErrorCode::DeadlineExceeded => TerminalPipelineSnapshotErrorCode::Timeout,
        ErrorCode::Unavailable => TerminalPipelineSnapshotErrorCode::Offline,
        _ => TerminalPipelineSnapshotErrorCode::RpcError,
    }
}

fn error_message(error: &ConnectError) -> String {
    error
        .message
        .clone()
        .unwrap_or_else(|| error.code.as_str().to_owned())
}

fn elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}
