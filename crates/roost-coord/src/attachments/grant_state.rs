//! The separate attachment-grant authority's immutable lease records, its
//! invalidation vocabulary, the request shape it admits, and the exact-worker
//! predicate every grant, peer and status fence shares. `AttachmentGrantOwner`
//! owns the mutations; attachment grants never share a registry, secret or lease
//! identity with terminal grants. Ports
//! `apps/coord/src/attachments/attachment-grant-owner-state.ts`.

use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode};
use roost_protocol::wire::WorkerFp;

use crate::attachments::transfer_limits::{is_bounded_identifier, is_opaque_upload_id};
use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};

/// The bound on every identifier a grant request names.
pub const ATTACHMENT_GRANT_MAX_IDENTIFIER_UTF8_BYTES: usize = 128;
/// The bound on the upload's filename.
pub const ATTACHMENT_GRANT_MAX_FILENAME_UTF8_BYTES: usize = 1024;

/// Why a worker's grants are retired wholesale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentGrantRetireReason {
    /// An operator deleted the machine.
    WorkerDeleted,
    /// The machine's credential was revoked.
    WorkerRevoked,
}

impl AttachmentGrantRetireReason {
    /// v2's spelling, for the retirement log line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkerDeleted => "worker_deleted",
            Self::WorkerRevoked => "worker_revoked",
        }
    }
}

/// Why a lease (or a whole worker's leases) stopped being authoritative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentGrantInvalidationKind {
    /// The lease outlived its TTL.
    GrantExpired,
    /// The lease's device was revoked.
    DeviceRevoked,
    /// The lease's worker was deleted or revoked.
    WorkerRetired,
}

/// The one immutable upload a grant authorizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentGrantDescriptor {
    /// The session whose attachment directory receives the upload.
    pub session_id: String,
    /// The browser-minted upload identity.
    pub upload_id: String,
    /// The name the upload is stored under.
    pub filename: String,
    /// Whether the worker stores it under a short path.
    pub short_path: bool,
    /// The exact size the transfer must deliver.
    pub total_bytes: u64,
}

/// One live grant, bound to the exact worker generation that acknowledged it.
#[derive(Debug, Clone)]
pub struct AttachmentGrantLease {
    /// The grant's identity.
    pub grant_id: String,
    /// The account-and-device owner key (`Principal::capture_owner_key`).
    pub owner_key: String,
    /// The device that minted it.
    pub device_fingerprint: String,
    /// The browser document that minted it.
    pub tab_id: String,
    /// The worker that holds its digest.
    pub worker_fp: String,
    /// The worker process epoch that acknowledged it.
    pub worker_epoch: String,
    /// The upload it authorizes.
    pub descriptor: AttachmentGrantDescriptor,
    /// When the coordinator forgets it, in epoch milliseconds.
    pub expires_at_ms: i64,
    /// The exact generation that acknowledged it; a replacement is not it.
    pub worker_handle: Arc<WorkerHandle>,
}

/// One invalidation announced to the grant's subscribers.
#[derive(Debug, Clone)]
pub struct AttachmentGrantInvalidation {
    /// Why.
    pub kind: AttachmentGrantInvalidationKind,
    /// The dropped lease; `None` for a worker retirement that dropped none.
    pub lease: Option<AttachmentGrantLease>,
    /// The worker the invalidation names.
    pub worker_fp: String,
    /// The revoked device, for a device revocation.
    pub device_fingerprint: Option<String>,
}

/// A subscriber to grant invalidations.
pub type AttachmentGrantInvalidationListener =
    Box<dyn Fn(&AttachmentGrantInvalidation) + Send + Sync>;

/// What peer signaling may read of the grant authority: a live exact lease,
/// and its invalidations.
pub trait AttachmentGrantPort: Send + Sync {
    /// The lease, only while it names this owner, tab and worker and its
    /// acknowledging generation is still current.
    fn owned_grant(
        &self,
        owner_key: &str,
        tab_id: &str,
        worker_fp: &str,
        grant_id: &str,
    ) -> Option<AttachmentGrantLease>;

    /// Hear every later invalidation, synchronously.
    fn subscribe_invalidation(&self, listener: AttachmentGrantInvalidationListener);
}

/// One grant a browser asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentGrantRequest {
    /// The caller's owner key.
    pub owner_key: String,
    /// The caller's device.
    pub device_fingerprint: String,
    /// The authenticated tab.
    pub tab_id: String,
    /// The worker the upload goes to.
    pub worker_fp: String,
    /// The upload.
    pub descriptor: AttachmentGrantDescriptor,
}

/// A minted grant: the lease and the plaintext secret only the browser learns.
#[derive(Debug, Clone)]
pub struct AttachmentGrantResult {
    /// The installed lease.
    pub lease: AttachmentGrantLease,
    /// 32 random bytes as lowercase hex; the worker holds only its digest.
    pub secret: String,
}

/// Whether `handle` is still the routable generation for its worker and still
/// the process epoch a fence captured. v2 writes this predicate twice
/// (`isExactAttachmentGrantWorker` and `isCurrentAttachmentPeerWorker`), and
/// both reduce to the same registry question here.
#[must_use]
pub fn is_exact_attachment_worker(
    workers: &WorkerRegistry,
    handle: &Arc<WorkerHandle>,
    worker_epoch: &str,
) -> bool {
    handle.process_epoch.as_deref() == Some(worker_epoch)
        && workers
            .current_routable(&handle.worker_fp)
            .is_some_and(|current| Arc::ptr_eq(&current, handle))
}

/// The routable generation for a fingerprint a request names as a string.
#[must_use]
pub fn current_routable_by_name(
    workers: &WorkerRegistry,
    worker_fp: &str,
) -> Option<Arc<WorkerHandle>> {
    let worker_fp = WorkerFp::try_from(worker_fp.to_owned()).ok()?;
    workers.current_routable(&worker_fp)
}

/// Refuse a request whose identifiers or descriptor are out of shape.
pub fn assert_attachment_grant_request(
    request: &AttachmentGrantRequest,
) -> Result<(), ConnectError> {
    for (field, value) in [
        ("owner", &request.owner_key),
        ("device", &request.device_fingerprint),
        ("tab", &request.tab_id),
        ("worker", &request.worker_fp),
        ("session", &request.descriptor.session_id),
    ] {
        if !is_bounded_identifier(value, ATTACHMENT_GRANT_MAX_IDENTIFIER_UTF8_BYTES) {
            return Err(attachment_grant_invalid(&format!(
                "attachment grant {field} is invalid"
            )));
        }
    }
    let descriptor = &request.descriptor;
    if !is_opaque_upload_id(
        &descriptor.upload_id,
        ATTACHMENT_GRANT_MAX_IDENTIFIER_UTF8_BYTES,
    ) || !is_bounded_identifier(
        &descriptor.filename,
        ATTACHMENT_GRANT_MAX_FILENAME_UTF8_BYTES,
    ) || descriptor.total_bytes > crate::attachments::transfer_limits::MAX_SAFE_INTEGER
    {
        return Err(attachment_grant_invalid(
            "attachment grant descriptor is invalid",
        ));
    }
    Ok(())
}

/// `InvalidArgument`, v2's grant-shape refusal.
#[must_use]
pub fn attachment_grant_invalid(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::InvalidArgument, message)
}

/// `PermissionDenied`, v2's grant-authority refusal.
#[must_use]
pub fn attachment_grant_denied(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::PermissionDenied, message)
}

/// `Unavailable`, v2's retryable grant refusal.
#[must_use]
pub fn attachment_grant_unavailable(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::Unavailable, message)
}

/// `ResourceExhausted`, v2's grant-capacity refusal.
#[must_use]
pub fn attachment_grant_exhausted(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::ResourceExhausted, message)
}
