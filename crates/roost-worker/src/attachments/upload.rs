//! The attachment-upload surface both carriers use: a coordinator relay chunk
//! settled into v2's `rpc-ok`/`rpc-error` shape, a direct chunk acknowledged
//! only after its flush, an operation's durable status, and releasing a dead
//! direct carrier. Ports v2 `apps/worker/src/attachments/attachment-upload.ts`,
//! including its 60 s idle sweep. Built once by `runtime::owners`; called by
//! `attachments::link` (relay) and the direct sockets.

use std::sync::Arc;
use std::time::Duration;

use roost_proto::DAttachmentChunk;
use tokio::task::JoinHandle;

use super::journal::sync_attachment_operation_progress;
use super::operation_owner::{AttachmentOperationChunk, AttachmentOperationOwner};
use super::receipts::{
    AttachmentOperationError, AttachmentOperationReceipt, AttachmentOperationStatus,
};
use super::store_paths::AttachmentBase;
use super::{AttachmentClock, Carrier, OperationDescriptor};
use crate::uplink::OwnerFuture;

/// How often abandoned operations are looked for.
pub const IDLE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// One chunk from an admitted direct carrier: the hello bound its total, and
/// the frame carries an exact offset and the chunk's own digest.
#[derive(Debug, Clone)]
pub struct DirectChunk {
    pub upload_id: String,
    pub session_id: String,
    pub filename: String,
    pub short_path: bool,
    pub total_bytes: u64,
    pub data: Vec<u8>,
    pub last: bool,
    pub seq: u32,
    pub offset: u64,
    pub chunk_sha256: String,
    /// The carrier's socket id; only this carrier may continue the operation.
    pub carrier_id: String,
}

/// What a direct carrier acknowledges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectChunkOutcome {
    /// A non-final chunk, flushed with its journal.
    Progress(AttachmentOperationReceipt),
    Committed {
        abs_path: String,
        receipt: AttachmentOperationReceipt,
    },
    Failed(AttachmentOperationError),
}

/// What the coordinator is told about one relayed chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayChunkOutcome {
    /// A non-final chunk was written; a relay acknowledges nothing until the end.
    Progress,
    /// v2 `rpc-ok { abs_path }`.
    Saved { abs_path: String },
    /// v2 `rpc-error` with [`AttachmentOperationError::message`].
    Failed(AttachmentOperationError),
}

/// The one attachment operation owner, shared by every carrier.
#[derive(Debug, Clone)]
pub struct AttachmentOperations {
    owner: Arc<AttachmentOperationOwner>,
}

impl AttachmentOperations {
    pub fn new(base: AttachmentBase, clock: AttachmentClock) -> Self {
        Self {
            owner: AttachmentOperationOwner::new(base, clock),
        }
    }

    /// v2 `handleAttachmentChunk`. The chunk is written in THIS call, before
    /// anything awaits, so the link's arrival order is the file's byte order;
    /// only a final chunk's commit is left to the returned future.
    pub fn accept_relay_chunk(&self, chunk: DAttachmentChunk) -> OwnerFuture<RelayChunkOutcome> {
        let DAttachmentChunk {
            request_id,
            session_id,
            filename,
            short_path,
            data,
            last,
            seq,
            ..
        } = chunk;
        // A relay chunk carries no offset: its position is the operation's
        // written length.
        let offset = if seq == 0 {
            0
        } else {
            self.owner.status(&session_id, &request_id).bytes_received
        };
        let accepted = self.owner.accept(AttachmentOperationChunk {
            descriptor: OperationDescriptor {
                request_id,
                session_id,
                filename,
                short_path,
                total_bytes: None,
            },
            carrier: Carrier::Coordinator,
            carrier_id: String::new(),
            seq,
            offset,
            data,
            last,
            chunk_sha256: None,
        });
        Box::pin(async move {
            match accepted.outcome().await {
                Err(error) => RelayChunkOutcome::Failed(error),
                Ok(receipt) if receipt.committed => RelayChunkOutcome::Saved {
                    abs_path: receipt.abs_path,
                },
                Ok(_) => RelayChunkOutcome::Progress,
            }
        })
    }

    /// v2 `handleDirectAttachmentChunk`: a non-final receipt is sent only after
    /// its bytes and journal reach the disk, and those flushes never block a
    /// runtime thread.
    pub async fn accept_direct_chunk(&self, chunk: DirectChunk) -> DirectChunkOutcome {
        let session_id = chunk.session_id.clone();
        let upload_id = chunk.upload_id.clone();
        let accepted = self.owner.accept(AttachmentOperationChunk {
            descriptor: OperationDescriptor {
                request_id: chunk.upload_id,
                session_id: chunk.session_id,
                filename: chunk.filename,
                short_path: chunk.short_path,
                total_bytes: Some(chunk.total_bytes),
            },
            carrier: Carrier::Direct,
            carrier_id: chunk.carrier_id,
            seq: chunk.seq,
            offset: chunk.offset,
            data: chunk.data,
            last: chunk.last,
            chunk_sha256: Some(chunk.chunk_sha256),
        });
        let receipt = match accepted.outcome().await {
            Err(error) => return DirectChunkOutcome::Failed(error),
            Ok(receipt) => receipt,
        };
        if receipt.committed {
            return DirectChunkOutcome::Committed {
                abs_path: receipt.abs_path.clone(),
                receipt,
            };
        }
        if let Err(error) =
            sync_attachment_operation_progress(&self.owner.base, &session_id, &upload_id).await
        {
            tracing::warn!(%upload_id, %error, "a direct attachment chunk could not be flushed");
            return DirectChunkOutcome::Failed(AttachmentOperationError::WriteFailed);
        }
        DirectChunkOutcome::Progress(receipt)
    }

    pub fn status(&self, session_id: &str, upload_id: &str) -> AttachmentOperationStatus {
        self.owner.status(session_id, upload_id)
    }

    /// A dead direct carrier releases only its file; its durable status stays
    /// queryable.
    pub fn detach_direct_carrier(&self, carrier_id: &str) {
        self.owner.detach_direct_carrier(carrier_id);
    }

    pub fn sweep_idle(&self) {
        self.owner.sweep_idle();
    }

    /// v2's `setInterval(sweepIdle, 60_000)`. The handle is the composition's
    /// to abort at shutdown.
    pub fn spawn_idle_sweep(&self) -> JoinHandle<()> {
        let owner = Arc::clone(&self.owner);
        tokio::spawn(async move {
            let start = tokio::time::Instant::now() + IDLE_SWEEP_INTERVAL;
            let mut ticks = tokio::time::interval_at(start, IDLE_SWEEP_INTERVAL);
            loop {
                ticks.tick().await;
                let owner = Arc::clone(&owner);
                // The sweep fails journals synchronously, as v2's does.
                if let Err(error) = tokio::task::spawn_blocking(move || owner.sweep_idle()).await {
                    tracing::warn!(%error, "the attachment idle sweep did not finish");
                }
            }
        })
    }
}
