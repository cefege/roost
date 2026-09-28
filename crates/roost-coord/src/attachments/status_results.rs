//! Bounded typed receipt-status requests from the authenticated RPC boundary
//! to one exact worker generation. Only metadata and the durable receipt pass
//! through; attachment bytes never enter this owner or the coordinator path, and
//! a missing or malformed status is a worker protocol failure, never synthesized.
//! Built once on `AttachmentsRuntime`; asked by `rpc_direct`, settled by
//! `worker_link::direct_results`. Ports `apps/coord/src/attachments/attachment-direct-status-results.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use connectrpc::{ConnectError, ErrorCode};
use roost_proto::{AttachmentTransferStatus, WAttachmentDirectStatus};
use tokio::sync::oneshot;

use crate::attachments::transfer_limits::{
    ATTACHMENT_TRANSFER_ERROR_REASONS, ATTACHMENT_TRANSFER_MAX_PENDING_STATUS_REQUESTS,
    ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS, MAX_SAFE_INTEGER, is_attachment_transfer_chunk_sha256,
    is_bounded_identifier, is_opaque_upload_id,
};
use crate::coord_core::ids::{draw, render_v4};
use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::workers::attachment_send::send_attachment_direct_status_request;

const STATUS_IDENTIFIER_MAX_UTF8_BYTES: usize = 128;

type StatusOutcome = Result<AttachmentTransferStatus, ConnectError>;

/// The typed status table; caller authorization stays at the RPC boundary.
#[derive(Debug)]
pub struct AttachmentDirectStatusResults {
    workers: Arc<WorkerRegistry>,
    pending: Mutex<HashMap<String, PendingStatus>>,
}

#[derive(Debug)]
struct PendingStatus {
    worker: Arc<WorkerHandle>,
    connection_generation: String,
    upload_id: String,
    settle: oneshot::Sender<StatusOutcome>,
}

/// Removes an entry whose waiter is gone, so a late answer settles nothing.
struct StatusRelease<'a> {
    owner: &'a AttachmentDirectStatusResults,
    request_id: String,
}

impl Drop for StatusRelease<'_> {
    fn drop(&mut self) {
        self.owner.table().remove(&self.request_id);
    }
}

impl AttachmentDirectStatusResults {
    /// An owner over the process's worker registry.
    #[must_use]
    pub fn new(workers: Arc<WorkerRegistry>) -> Self {
        Self {
            workers,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Ask `worker` for one upload's durable receipt and wait for the typed
    /// answer, the generation's end, or the status deadline.
    pub async fn request(
        &self,
        worker: &Arc<WorkerHandle>,
        session_id: &str,
        upload_id: &str,
    ) -> StatusOutcome {
        assert_status_request_shape(session_id, upload_id)?;
        let current = self.workers.current_routable(&worker.worker_fp);
        if !current.is_some_and(|current| Arc::ptr_eq(&current, worker)) {
            return Err(status_unavailable(
                "attachment status worker is unavailable",
            ));
        }
        let (request_id, settled) = self.reserve(worker, upload_id)?;
        let _release = StatusRelease {
            owner: self,
            request_id: request_id.clone(),
        };
        let sent = send_attachment_direct_status_request(
            &self.workers,
            worker,
            &request_id,
            session_id,
            upload_id,
        );
        if sent {
            tracing::debug!(worker_fp = %worker.worker_fp, pending = self.table().len(),
                "attachment status: status_requested");
        } else {
            self.cancel_pending(
                &request_id,
                status_unavailable("attachment status worker is unavailable"),
            );
        }
        let deadline = Duration::from_millis(ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS);
        match tokio::time::timeout(deadline, settled).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(status_unavailable("attachment status is unavailable")),
            Err(_) => {
                tracing::debug!(worker_fp = %worker.worker_fp, "attachment status: status_timed_out");
                Err(ConnectError::new(
                    ErrorCode::DeadlineExceeded,
                    "attachment status timed out",
                ))
            }
        }
    }

    /// Settle one pending request from the exact generation it was sent to.
    /// `false` only when nothing waits under that identity from that source.
    pub fn accept_status(
        &self,
        source: &Arc<WorkerHandle>,
        result: &WAttachmentDirectStatus,
    ) -> bool {
        let valid = {
            let table = self.table();
            let Some(pending) = table.get(&result.request_id) else {
                return false;
            };
            if !self.matches_pending(source, pending) {
                return false;
            }
            result
                .status
                .as_option()
                .filter(|status| is_valid_status(status, &pending.upload_id))
                .cloned()
        };
        let Some(status) = valid else {
            self.cancel_pending(
                &result.request_id,
                status_unavailable("attachment status worker response is invalid"),
            );
            return true;
        };
        let Some(pending) = self.table().remove(&result.request_id) else {
            return false;
        };
        let _ = pending.settle.send(Ok(status));
        tracing::debug!(worker_fp = %source.worker_fp, pending = self.table().len(),
            "attachment status: status_accepted");
        true
    }

    /// Fail every request the ended generation would have answered.
    pub fn cancel_for_worker_handle(&self, worker: &Arc<WorkerHandle>, reason: &str) {
        let doomed: Vec<String> = self
            .table()
            .iter()
            .filter(|(_, pending)| Arc::ptr_eq(&pending.worker, worker))
            .map(|(request_id, _)| request_id.clone())
            .collect();
        for request_id in doomed {
            self.cancel_pending(
                &request_id,
                status_unavailable("attachment status worker connection changed"),
            );
        }
        tracing::debug!(worker_fp = %worker.worker_fp, reason, "attachment status: worker_handle_cancelled");
    }

    fn reserve(
        &self,
        worker: &Arc<WorkerHandle>,
        upload_id: &str,
    ) -> Result<(String, oneshot::Receiver<StatusOutcome>), ConnectError> {
        let mut table = self.table();
        if table.len() >= ATTACHMENT_TRANSFER_MAX_PENDING_STATUS_REQUESTS {
            return Err(status_exhausted());
        }
        let request_id = (0..8)
            .filter_map(|_| draw::<16>().ok().map(render_v4))
            .find(|candidate| !table.contains_key(candidate))
            .ok_or_else(status_exhausted)?;
        let (settle, settled) = oneshot::channel();
        table.insert(
            request_id.clone(),
            PendingStatus {
                worker: Arc::clone(worker),
                connection_generation: worker.connection_generation.clone(),
                upload_id: upload_id.to_owned(),
                settle,
            },
        );
        Ok((request_id, settled))
    }

    fn matches_pending(&self, source: &Arc<WorkerHandle>, pending: &PendingStatus) -> bool {
        Arc::ptr_eq(source, &pending.worker)
            && source.connection_generation == pending.connection_generation
            && self
                .workers
                .current_routable(&source.worker_fp)
                .is_some_and(|current| Arc::ptr_eq(&current, source))
    }

    fn cancel_pending(&self, request_id: &str, error: ConnectError) {
        let Some(pending) = self.table().remove(request_id) else {
            return;
        };
        let _ = pending.settle.send(Err(error));
        tracing::debug!(worker_fp = %pending.worker.worker_fp, pending = self.table().len(),
            "attachment status: status_cancelled");
    }

    fn table(&self) -> MutexGuard<'_, HashMap<String, PendingStatus>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn assert_status_request_shape(session_id: &str, upload_id: &str) -> Result<(), ConnectError> {
    if is_bounded_identifier(session_id, STATUS_IDENTIFIER_MAX_UTF8_BYTES)
        && is_opaque_upload_id(upload_id, STATUS_IDENTIFIER_MAX_UTF8_BYTES)
    {
        return Ok(());
    }
    Err(ConnectError::new(
        ErrorCode::InvalidArgument,
        "attachment status request is invalid",
    ))
}

/// A receipt the worker may truthfully have sent for `upload_id`.
fn is_valid_status(status: &AttachmentTransferStatus, upload_id: &str) -> bool {
    status.upload_id == upload_id
        && status.bytes_received <= MAX_SAFE_INTEGER
        && (status.last_chunk_sha256.is_empty()
            || is_attachment_transfer_chunk_sha256(&status.last_chunk_sha256))
        && (status.error.is_empty()
            || ATTACHMENT_TRANSFER_ERROR_REASONS.contains(&status.error.as_str()))
        && (!status.committed || status.error.is_empty())
        && (status.committed || status.abs_path.is_empty())
}

fn status_unavailable(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::Unavailable, message)
}

fn status_exhausted() -> ConnectError {
    ConnectError::new(
        ErrorCode::ResourceExhausted,
        "attachment status capacity is exhausted",
    )
}
