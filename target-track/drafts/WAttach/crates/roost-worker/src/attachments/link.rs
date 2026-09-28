//! The coordinator link's attachment owner: relayed upload chunks, the direct
//! grants the coordinator installs and revokes, and the durable status it asks
//! for on a browser's behalf. Ports the attachment callbacks of v2
//! `transport/coord-link-deps.ts` (`onAttachmentChunk`, the grant pair),
//! `coord-link-direct-deps.ts` (`onAttachmentDirectStatusRequest`) and
//! `boot/boot-local-terminal.ts` (`revokeAttachmentDevice`). Built by `runtime::owners`.

use std::sync::Arc;

use roost_proto::{
    AttachmentTransferStatus, DAttachmentChunk, DAttachmentDirectStatusRequest,
    DLocalAttachmentGrant,
};

use super::direct_owners::AttachmentDirect;
use super::grants::AttachmentGrantStore;
use super::upload::{AttachmentOperations, RelayChunkOutcome};
use crate::link_ports::AttachmentLinkPort;
use crate::uplink::OwnerFuture;

/// One per process: the operation owner and grant store every carrier shares.
#[derive(Debug, Clone)]
pub struct AttachmentLink {
    operations: AttachmentOperations,
    grants: Arc<AttachmentGrantStore>,
    direct: AttachmentDirect,
}

impl AttachmentLink {
    pub fn new(
        operations: AttachmentOperations,
        grants: Arc<AttachmentGrantStore>,
        direct: AttachmentDirect,
    ) -> Self {
        Self {
            operations,
            grants,
            direct,
        }
    }
}

impl AttachmentLinkPort for AttachmentLink {
    fn accept_relay_chunk(&self, chunk: DAttachmentChunk) -> OwnerFuture<RelayChunkOutcome> {
        self.operations.accept_relay_chunk(chunk)
    }

    fn install_grant(&self, request: &DLocalAttachmentGrant) -> Result<(), String> {
        self.grants.install(request).map(|_| ())
    }

    /// v2 `revokeAttachmentDevice`: the device's sockets fail first, then its
    /// grants go, then its pending peer negotiations.
    fn revoke_device(&self, device_fingerprint: &str) {
        self.direct.revoke_sockets(device_fingerprint);
        let revoked = self.grants.revoke_device(device_fingerprint);
        self.direct.revoke_peers(device_fingerprint);
        tracing::info!(grants = revoked, "the coordinator revoked a device's attachment authority");
    }

    fn direct_status(&self, request: &DAttachmentDirectStatusRequest) -> AttachmentTransferStatus {
        self.operations
            .status(&request.session_id, &request.upload_id)
            .to_proto()
    }
}
