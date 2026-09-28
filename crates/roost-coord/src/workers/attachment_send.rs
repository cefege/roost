//! The direct-attachment control senders: install and revoke a grant digest,
//! offer and cancel an attachment peer, and ask for a durable receipt -- each
//! written only through the exact worker generation its caller captured.
//! Ports `apps/coord/src/attachments/worker-send-attachment-{grant,peer,status}.ts`.
//! Called by `attachments::{grant, peer, status_results}`; every write goes
//! through `workers::send::send_frame_through`, so no bytes or secrets pass here.

use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode};
use roost_proto::{
    DAttachmentDirectStatusRequest, DLocalAttachmentGrant, DLocalAttachmentGrantRevoke,
    DLocalAttachmentPeerCancel, DLocalAttachmentPeerOffer,
};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

use crate::attachments::grant_state::{AttachmentGrantDescriptor, is_exact_attachment_worker};
use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::terminal_screen::pending_rpcs::{PendingRpc, PendingRpcs};
use crate::workers::send::{SendOutcome, send_frame_through};

/// One grant install: the digest and the immutable descriptor, never the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAttachmentGrantInstall {
    /// The grant's identity.
    pub grant_id: String,
    /// Lowercase hex SHA-256 of the browser's secret.
    pub secret_sha256: String,
    /// The upload it authorizes.
    pub descriptor: AttachmentGrantDescriptor,
    /// The minting device.
    pub device_fingerprint: String,
    /// The minting tab.
    pub tab_id: String,
    /// How long the worker honours it.
    pub ttl_ms: u32,
}

/// One peer offer, sent after its typed waiter is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentPeerOfferSend {
    /// The correlation id the answer echoes.
    pub request_id: String,
    /// The grant the offer spends.
    pub grant_id: String,
    /// The browser's peer identity.
    pub peer_id: String,
    /// The offering device.
    pub device_fingerprint: String,
    /// The offering tab.
    pub tab_id: String,
    /// The process epoch the browser dialled.
    pub worker_epoch: String,
    /// The browser's SDP offer.
    pub offer_sdp: String,
    /// What is left of the answer deadline for the worker.
    pub budget_ms: u32,
    /// The operator's STUN servers.
    pub stun_urls: Vec<String>,
}

/// Install one grant on the captured generation and open its acknowledgement
/// wait: the worker answers `rpc-ok` once the digest is stored.
///
/// A generation that is no longer exact refuses before anything is correlated
/// (v2 "worker offline"); a socket that drops the frame settles the wait as
/// `Unavailable` at once rather than leaving it to the deadline.
pub fn send_local_attachment_grant_request(
    workers: &WorkerRegistry,
    pending_rpcs: &Arc<PendingRpcs>,
    worker: &Arc<WorkerHandle>,
    worker_epoch: &str,
    message: LocalAttachmentGrantInstall,
    now_ms: i64,
) -> Result<PendingRpc, ConnectError> {
    if !is_exact_attachment_worker(workers, worker, worker_epoch) {
        return Err(ConnectError::new(ErrorCode::Unavailable, "worker offline"));
    }
    let worker_fp = worker.worker_fp.as_str();
    let pending = pending_rpcs.create_fresh(Some(worker_fp), now_ms)?;
    let descriptor = message.descriptor;
    let frame = CoordWorkerDownstream::LocalAttachmentGrant(DLocalAttachmentGrant {
        request_id: pending.request_id().to_owned(),
        grant_id: message.grant_id,
        secret_sha256: message.secret_sha256,
        session_id: descriptor.session_id,
        upload_id: descriptor.upload_id,
        filename: descriptor.filename,
        short_path: descriptor.short_path,
        total_bytes: descriptor.total_bytes,
        device_fingerprint: message.device_fingerprint,
        tab_id: message.tab_id,
        ttl_ms: message.ttl_ms,
        worker_epoch: worker_epoch.to_owned(),
        ..Default::default()
    });
    if let SendOutcome::Refused(refusal) = send_frame_through(workers, worker, frame) {
        tracing::warn!(%worker_fp, %refusal, "attachment send: the grant install was not written");
        pending_rpcs.reject_unavailable(
            pending.request_id(),
            "worker transport unavailable",
            Some(worker_fp),
        );
    }
    Ok(pending)
}

/// Tell one exact generation to forget every grant a revoked device holds.
pub fn send_local_attachment_grant_revoke(
    workers: &WorkerRegistry,
    worker: &Arc<WorkerHandle>,
    device_fingerprint: &str,
) -> bool {
    let Some(epoch) = worker.process_epoch.as_deref() else {
        return false;
    };
    if !is_exact_attachment_worker(workers, worker, epoch) {
        return false;
    }
    let frame = CoordWorkerDownstream::LocalAttachmentGrantRevoke(DLocalAttachmentGrantRevoke {
        device_fingerprint: device_fingerprint.to_owned(),
        ..Default::default()
    });
    send_frame_through(workers, worker, frame).is_admitted()
}

/// Send one offer to the exact generation its waiter names.
pub fn send_attachment_peer_offer(
    workers: &WorkerRegistry,
    worker: &Arc<WorkerHandle>,
    message: AttachmentPeerOfferSend,
) -> bool {
    if !is_exact_attachment_worker(workers, worker, &message.worker_epoch) {
        return false;
    }
    let frame = CoordWorkerDownstream::LocalAttachmentPeerOffer(DLocalAttachmentPeerOffer {
        request_id: message.request_id,
        connection_generation: worker.connection_generation.clone(),
        worker_epoch: message.worker_epoch,
        grant_id: message.grant_id,
        peer_id: message.peer_id,
        device_fingerprint: message.device_fingerprint,
        tab_id: message.tab_id,
        offer_sdp: message.offer_sdp,
        budget_ms: message.budget_ms,
        stun_urls: message.stun_urls,
        ..Default::default()
    });
    send_frame_through(workers, worker, frame).is_admitted()
}

/// Cancel one offer, only while the captured generation is still current: a
/// replacement never hears about its predecessor's negotiations.
pub fn send_attachment_peer_cancel(
    workers: &WorkerRegistry,
    worker: &Arc<WorkerHandle>,
    request_id: &str,
    peer_id: &str,
    worker_epoch: &str,
) -> bool {
    if !is_exact_attachment_worker(workers, worker, worker_epoch) {
        return false;
    }
    let frame = CoordWorkerDownstream::LocalAttachmentPeerCancel(DLocalAttachmentPeerCancel {
        request_id: request_id.to_owned(),
        connection_generation: worker.connection_generation.clone(),
        worker_epoch: worker_epoch.to_owned(),
        peer_id: peer_id.to_owned(),
        ..Default::default()
    });
    send_frame_through(workers, worker, frame).is_admitted()
}

/// Ask the exact generation for one upload's durable receipt; the typed
/// answer settles through `AttachmentDirectStatusResults`, never `rpc-ok`.
pub fn send_attachment_direct_status_request(
    workers: &WorkerRegistry,
    worker: &Arc<WorkerHandle>,
    request_id: &str,
    session_id: &str,
    upload_id: &str,
) -> bool {
    let frame =
        CoordWorkerDownstream::AttachmentDirectStatusRequest(DAttachmentDirectStatusRequest {
            request_id: request_id.to_owned(),
            session_id: session_id.to_owned(),
            upload_id: upload_id.to_owned(),
            ..Default::default()
        });
    send_frame_through(workers, worker, frame).is_admitted()
}
