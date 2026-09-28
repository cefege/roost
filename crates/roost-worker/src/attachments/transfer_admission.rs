//! A direct carrier's hello admission: the hello's credential must prove its
//! dedicated grant, and a WebRTC port's hello must also match the tuple its
//! peer negotiation authorized. The admitted descriptor is immutable for the
//! port's life. Ports v2
//! `apps/worker/src/attachments/attachment-transfer-admission.ts`. Called by
//! the direct attachment sockets.

use roost_proto::AttachmentTransferHello;
use roost_protocol::attachment_transfer::TransferErrorReason;

use super::grants::{AttachmentGrant, AttachmentGrantCredential, AttachmentGrantStore};

/// What a WebRTC peer negotiation authorized, which its port's hello must
/// repeat exactly. A loopback socket has none, and its hello names no peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentPeerExpectedTuple {
    pub peer_id: String,
    pub grant_id: String,
    pub device_fingerprint: String,
    pub tab_id: String,
    pub worker_epoch: String,
}

/// The upload an admitted port may write, taken from the grant rather than the
/// hello, so nothing the browser sends after admission can widen it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentUploadMetadata {
    pub grant_id: String,
    pub device_fingerprint: String,
    pub tab_id: String,
    pub session_id: String,
    pub upload_id: String,
    pub filename: String,
    pub short_path: bool,
    pub total_bytes: u64,
    pub peer_id: String,
    pub worker_epoch: String,
}

/// v2 `admitAttachmentTransferHello`. Every refusal is `grant_unavailable`:
/// a hello learns nothing about which part of its credential was wrong.
pub fn admit_attachment_transfer_hello(
    hello: &AttachmentTransferHello,
    grants: &AttachmentGrantStore,
    expected_peer: Option<&AttachmentPeerExpectedTuple>,
) -> Result<AttachmentUploadMetadata, TransferErrorReason> {
    let verdict = grants.verify(&AttachmentGrantCredential {
        grant_id: hello.grant_id.clone(),
        secret: hello.secret.clone(),
        session_id: hello.session_id.clone(),
        upload_id: hello.upload_id.clone(),
        filename: hello.filename.clone(),
        short_path: hello.short_path,
        total_bytes: hello.total_bytes,
        device_fingerprint: hello.device_fingerprint.clone(),
        tab_id: hello.tab_id.clone(),
        worker_epoch: hello.worker_epoch.clone(),
    });
    let grant = match verdict {
        Ok(grant) if matches_expected_peer(expected_peer, hello) => grant,
        Ok(_) => {
            tracing::info!("an attachment hello did not match its peer negotiation");
            return Err(TransferErrorReason::GrantUnavailable);
        }
        Err(reason) => {
            tracing::info!(reason, "an attachment hello was refused");
            return Err(TransferErrorReason::GrantUnavailable);
        }
    };
    Ok(AttachmentUploadMetadata {
        grant_id: grant.grant_id,
        device_fingerprint: grant.device_fingerprint,
        tab_id: grant.tab_id,
        session_id: grant.session_id,
        upload_id: grant.upload_id,
        filename: grant.filename,
        short_path: grant.short_path,
        total_bytes: grant.total_bytes,
        peer_id: hello.peer_id.clone(),
        worker_epoch: grant.worker_epoch,
    })
}

/// v2 `attachmentMetadataMatchesGrant`: whether a renewed or replaced grant
/// still authorizes exactly what the port was admitted for.
pub fn attachment_metadata_matches_grant(
    metadata: &AttachmentUploadMetadata,
    grant: &AttachmentGrant,
) -> bool {
    grant.grant_id == metadata.grant_id
        && grant.worker_epoch == metadata.worker_epoch
        && grant.device_fingerprint == metadata.device_fingerprint
        && grant.tab_id == metadata.tab_id
        && grant.session_id == metadata.session_id
        && grant.upload_id == metadata.upload_id
        && grant.filename == metadata.filename
        && grant.short_path == metadata.short_path
        && grant.total_bytes == metadata.total_bytes
}

fn matches_expected_peer(
    expected: Option<&AttachmentPeerExpectedTuple>,
    hello: &AttachmentTransferHello,
) -> bool {
    let Some(expected) = expected else {
        return hello.peer_id.is_empty();
    };
    expected.peer_id == hello.peer_id
        && expected.grant_id == hello.grant_id
        && expected.device_fingerprint == hello.device_fingerprint
        && expected.tab_id == hello.tab_id
        && expected.worker_epoch == hello.worker_epoch
}
