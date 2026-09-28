//! The direct-terminal authorization frames one exact worker generation
//! receives: the grant install whose ACK the grant owner awaits before a
//! browser learns its secret, and the best-effort revoke and retirement.
//! Ports `apps/coord/src/terminal/direct/worker-send-local-terminal.ts`. Called by
//! `terminal_direct::grant_owner` and `grant_refresh`; the install correlates
//! through `services.scrollback.pending()`. Only secret digests pass through.

use std::sync::Arc;
use std::time::Duration;

use connectrpc::{ConnectError, ErrorCode};
use roost_proto::{DLocalTerminalGrant, DLocalTerminalGrantRevoke, DTerminalDirectRetire};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::terminal_direct::grant_state::TerminalDirectRetireReason;
use crate::terminal_screen::pending_rpcs::{PendingRpc, PendingRpcs};
use crate::workers::send::send_frame_through;

/// How long the owner waits for a worker to acknowledge one grant install.
pub const GRANT_ACK_TIMEOUT_MS: u64 = 10_000;

/// One grant as the worker installs it: the secret travels only as a digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTerminalGrantInstall {
    /// The stable grant id, reused across renewals on one worker epoch.
    pub grant_id: String,
    /// Hex SHA-256 of the browser secret.
    pub secret_sha256: String,
    /// The sessions the grant covers.
    pub session_ids: Vec<String>,
    /// The browser device the grant is bound to.
    pub device_fingerprint: String,
    /// The browser document the grant is bound to.
    pub tab_id: String,
    /// The worker's own enforcement lifetime.
    pub ttl_ms: u32,
}

/// An install the worker has been sent, waiting for its `rpc-ok`.
#[derive(Debug)]
pub struct PendingLocalTerminalGrantInstall {
    pending: PendingRpc,
    timeout_ms: u64,
}

impl PendingLocalTerminalGrantInstall {
    /// The correlation id the worker's ACK echoes.
    #[must_use]
    pub fn request_id(&self) -> &str {
        self.pending.request_id()
    }

    /// Wait for the worker's ACK, a rejection, or v2's pending-RPC deadline
    /// (`DeadlineExceeded`, "worker did not reply within …ms").
    pub async fn acknowledged(mut self) -> Result<serde_json::Value, ConnectError> {
        let timeout_ms = self.timeout_ms;
        match tokio::time::timeout(Duration::from_millis(timeout_ms), self.pending.settle()).await {
            Ok(settled) => settled,
            Err(_) => {
                tracing::warn!(
                    request_id = self.pending.request_id(),
                    worker_fp = self.pending.worker_fp(),
                    timeout_ms,
                    "a worker did not acknowledge a local terminal grant in time"
                );
                Err(ConnectError::new(
                    ErrorCode::DeadlineExceeded,
                    format!("worker did not reply within {timeout_ms}ms"),
                ))
            }
        }
    }
}

/// Whether `worker` is still the fingerprint's routable generation AND runs the
/// exact process epoch a grant was minted for (v2 `isExactRoutableWorker` and
/// `isExactTerminalGrantWorker`, one predicate here).
#[must_use]
pub fn is_exact_routable_worker(
    workers: &WorkerRegistry,
    worker: &Arc<WorkerHandle>,
    worker_epoch: Option<&str>,
) -> bool {
    worker.process_epoch.as_deref() == worker_epoch
        && workers
            .current_routable(&worker.worker_fp)
            .is_some_and(|current| Arc::ptr_eq(&current, worker))
}

/// Install a grant on the captured handle and worker boot epoch only.
///
/// The correlation entry opens before the write, so the ACK cannot beat it; a
/// write the socket refused settles it `Unavailable` at once.
pub fn send_local_terminal_grant_request(
    workers: &WorkerRegistry,
    pending_rpcs: &Arc<PendingRpcs>,
    worker: &Arc<WorkerHandle>,
    worker_epoch: Option<&str>,
    message: LocalTerminalGrantInstall,
    now_ms: i64,
) -> Result<PendingLocalTerminalGrantInstall, ConnectError> {
    if !is_exact_routable_worker(workers, worker, worker_epoch) {
        return Err(ConnectError::new(ErrorCode::Unavailable, "worker offline"));
    }
    let worker_fp = worker.worker_fp.as_str();
    let pending = pending_rpcs.create_fresh(Some(worker_fp), now_ms)?;
    let frame = CoordWorkerDownstream::LocalTerminalGrant(DLocalTerminalGrant {
        request_id: pending.request_id().to_owned(),
        grant_id: message.grant_id,
        secret_sha256: message.secret_sha256,
        session_ids: message.session_ids,
        device_fingerprint: message.device_fingerprint,
        tab_id: message.tab_id,
        ttl_ms: message.ttl_ms,
        worker_epoch: worker_epoch.unwrap_or_default().to_owned(),
        ..Default::default()
    });
    if !send_frame_through(workers, worker, frame).is_admitted() {
        tracing::warn!(
            worker_fp,
            "local terminal grant: the socket did not take the install"
        );
        pending_rpcs.reject_unavailable(
            pending.request_id(),
            "worker transport unavailable",
            Some(worker_fp),
        );
    }
    Ok(PendingLocalTerminalGrantInstall {
        pending,
        timeout_ms: GRANT_ACK_TIMEOUT_MS,
    })
}

/// Tell one captured worker generation to drop every grant for a device.
pub fn send_local_terminal_grant_revoke(
    workers: &WorkerRegistry,
    worker: &Arc<WorkerHandle>,
    device_fingerprint: &str,
) -> bool {
    let frame = CoordWorkerDownstream::LocalTerminalGrantRevoke(DLocalTerminalGrantRevoke {
        device_fingerprint: device_fingerprint.to_owned(),
        ..Default::default()
    });
    send_frame_through(workers, worker, frame).is_admitted()
}

/// Retire a worker's direct transport. Must be enqueued BEFORE the caller
/// fences the captured handle, or the fence silently eats it.
pub fn send_terminal_direct_retire(
    workers: &WorkerRegistry,
    worker: &Arc<WorkerHandle>,
    worker_epoch: &str,
    reason: TerminalDirectRetireReason,
) -> bool {
    if !is_exact_routable_worker(workers, worker, Some(worker_epoch)) {
        return false;
    }
    let frame = CoordWorkerDownstream::TerminalDirectRetire(DTerminalDirectRetire {
        worker_epoch: worker_epoch.to_owned(),
        reason: reason.as_str().to_owned(),
        ..Default::default()
    });
    send_frame_through(workers, worker, frame).is_admitted()
}
