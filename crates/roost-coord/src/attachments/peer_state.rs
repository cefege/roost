//! Pending attachment-peer records, the admission vocabulary, and the grant and
//! worker fences an offer passes before and after it is sent -- without mutable
//! registry state: `AttachmentPeerNegotiations` owns lifecycle and correlation.
//! Nothing here retains SDP, and signaling reuses only the grant lease tuple.
//! Ports `apps/coord/src/attachments/attachment-peer-negotiation-state.ts`.

use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode};
use roost_proto::{
    SessionsNegotiateAttachmentPeerRequest, SessionsNegotiateAttachmentPeerResponse,
};
use roost_protocol::terminal_peer::peer::TERMINAL_PEER_SDP_MAX_UTF8_BYTES;
use roost_protocol::versioning::CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1;
use tokio::sync::oneshot;

use crate::attachments::grant_state::{
    AttachmentGrantLease, AttachmentGrantPort, current_routable_by_name, is_exact_attachment_worker,
};
use crate::attachments::transfer_limits::is_bounded_identifier;
use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};

/// The bound on every identifier an attachment peer request names.
pub const ATTACHMENT_PEER_MAX_IDENTIFIER_UTF8_BYTES: usize = 128;

/// What the operator configured for peer signaling, read per call from the
/// boot config so the owner holds none of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentPeerSignalConfig {
    /// `terminal_peer_enabled`: direct peers are offered at all.
    pub enabled: bool,
    /// `terminal_peer_stun_urls`, handed to the worker with each offer.
    pub stun_urls: Vec<String>,
}

/// The authenticated browser a negotiation is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentPeerCaller {
    /// `Principal::capture_owner_key`.
    pub owner_key: String,
    /// The device's key fingerprint.
    pub device_fingerprint: String,
    /// The tab the request authenticated with (`x-roost-tab-id`).
    pub tab_id: Option<String>,
}

/// One negotiation between admission and its pending entry.
#[derive(Debug, Clone)]
pub(crate) struct PeerAdmission {
    pub(crate) peer_id: String,
    pub(crate) offer_digest: String,
    pub(crate) device_fingerprint: String,
    pub(crate) worker_fp: String,
}

/// One offer awaiting its worker's typed answer.
#[derive(Debug)]
pub(crate) struct PendingAttachmentPeer {
    pub(crate) key: PeerKey,
    pub(crate) device_fingerprint: String,
    pub(crate) grant_id: String,
    pub(crate) peer_id: String,
    pub(crate) offer_digest: String,
    pub(crate) worker: Arc<WorkerHandle>,
    pub(crate) connection_generation: String,
    pub(crate) worker_epoch: String,
    pub(crate) settle: oneshot::Sender<PeerOutcome>,
}

/// What a negotiation settles with.
pub(crate) type PeerOutcome = Result<SessionsNegotiateAttachmentPeerResponse, ConnectError>;

/// One browser document's negotiation slot against one worker.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PeerKey {
    pub(crate) owner_key: String,
    pub(crate) tab_id: String,
    pub(crate) worker_fp: String,
}

/// Refuse a request whose identifiers or offer are out of shape, before any
/// capacity is spent on it.
pub fn assert_attachment_peer_request_shape(
    request: &SessionsNegotiateAttachmentPeerRequest,
) -> Result<(), ConnectError> {
    for (field, value) in [
        ("worker_fp", &request.worker_fp),
        ("grant_id", &request.grant_id),
        ("tab_id", &request.tab_id),
        ("peer_id", &request.peer_id),
        ("worker_epoch", &request.worker_epoch),
    ] {
        if !is_bounded_identifier(value, ATTACHMENT_PEER_MAX_IDENTIFIER_UTF8_BYTES) {
            return Err(attachment_peer_error(
                ErrorCode::InvalidArgument,
                &format!("attachment peer {field} is invalid"),
            ));
        }
    }
    if !is_peer_uuid(&request.peer_id) {
        return Err(attachment_peer_error(
            ErrorCode::InvalidArgument,
            "attachment peer peer_id is invalid",
        ));
    }
    if request.offer_sdp.len() > TERMINAL_PEER_SDP_MAX_UTF8_BYTES {
        return Err(attachment_peer_error(
            ErrorCode::InvalidArgument,
            "attachment peer offer is invalid",
        ));
    }
    Ok(())
}

/// v2's `/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/iu`.
fn is_peer_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    let shaped = groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, length)| {
            group.len() == length && group.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
    shaped
        && matches!(groups[2].as_bytes()[0], b'1'..=b'8')
        && matches!(
            groups[3].as_bytes()[0].to_ascii_lowercase(),
            b'8' | b'9' | b'a' | b'b'
        )
}

/// The fixed code a worker's refusal becomes; only the three reasons a
/// browser acts on differently are distinguished.
pub fn attachment_peer_worker_failure(reason: &str) -> ConnectError {
    match reason {
        "grant_unavailable" | "expired" => attachment_peer_error(
            ErrorCode::PermissionDenied,
            "attachment peer worker rejected the grant",
        ),
        "capacity" => attachment_peer_error(
            ErrorCode::ResourceExhausted,
            "attachment peer worker capacity is exhausted",
        ),
        _ => attachment_peer_error(
            ErrorCode::Unavailable,
            "attachment peer worker is unavailable",
        ),
    }
}

/// One attachment-peer refusal.
#[must_use]
pub fn attachment_peer_error(code: ErrorCode, message: &str) -> ConnectError {
    ConnectError::new(code, message)
}

/// The caller's exact live lease for the request, or `PermissionDenied`.
pub(crate) fn require_owned_grant(
    grants: &dyn AttachmentGrantPort,
    owner_key: &str,
    request: &SessionsNegotiateAttachmentPeerRequest,
) -> Result<AttachmentGrantLease, ConnectError> {
    grants
        .owned_grant(
            owner_key,
            &request.tab_id,
            &request.worker_fp,
            &request.grant_id,
        )
        .ok_or_else(|| {
            attachment_peer_error(
                ErrorCode::PermissionDenied,
                "attachment peer grant is unavailable",
            )
        })
}

/// The same lease again, refused if its identity, generation, epoch or
/// upload changed since `expected` was read.
pub(crate) fn require_stable_grant(
    grants: &dyn AttachmentGrantPort,
    owner_key: &str,
    request: &SessionsNegotiateAttachmentPeerRequest,
    expected: &AttachmentGrantLease,
) -> Result<AttachmentGrantLease, ConnectError> {
    let current = require_owned_grant(grants, owner_key, request)?;
    let stable = current.grant_id == expected.grant_id
        && Arc::ptr_eq(&current.worker_handle, &expected.worker_handle)
        && current.worker_epoch == expected.worker_epoch
        && current.descriptor.session_id == expected.descriptor.session_id
        && current.descriptor.upload_id == expected.descriptor.upload_id;
    stable.then_some(current).ok_or_else(|| {
        attachment_peer_error(
            ErrorCode::PermissionDenied,
            "attachment peer grant changed during negotiation",
        )
    })
}

/// The grant's acknowledging generation, only while peers are enabled, it is
/// still current at the request's epoch, and it negotiated attachment peers.
pub(crate) fn require_worker(
    workers: &WorkerRegistry,
    request: &SessionsNegotiateAttachmentPeerRequest,
    grant: &AttachmentGrantLease,
    config: &AttachmentPeerSignalConfig,
) -> Result<Arc<WorkerHandle>, ConnectError> {
    current_routable_by_name(workers, &request.worker_fp)
        .filter(|worker| {
            config.enabled
                && Arc::ptr_eq(worker, &grant.worker_handle)
                && worker.process_epoch.as_deref() == Some(grant.worker_epoch.as_str())
                && grant.worker_epoch == request.worker_epoch
                && worker
                    .capabilities
                    .contains(CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1)
                && is_exact_attachment_worker(workers, worker, &request.worker_epoch)
        })
        .ok_or_else(|| {
            attachment_peer_error(
                ErrorCode::Unavailable,
                "attachment peer worker is unavailable",
            )
        })
}
