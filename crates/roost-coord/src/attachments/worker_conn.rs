//! The attachment half of the worker link's lifecycle: which hellos are told
//! `attachment-transfer-peer-webrtc-v1`, and which typed results a superseded,
//! revoked or closed generation's end fails. Kept apart from terminal peer
//! negotiation; grant authority stays in `AttachmentGrantOwner`.
//! Called by `worker_link::link_session` (acknowledgement) and registered on
//! `CoordServices::worker_lifecycle`. Ports `apps/coord/src/attachments/worker-conn-attachment.ts`.

use std::sync::Arc;

use roost_protocol::versioning::CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1;

use crate::attachments::peer::AttachmentPeerNegotiations;
use crate::attachments::status_results::AttachmentDirectStatusResults;
use crate::coord_core::boot_facts::BootFacts;
use crate::coord_core::worker_handle::WorkerHandle;
use crate::coord_core::worker_lifecycle::{LinkEnd, WorkerLifecycleObserver};

/// Acknowledge the attachment peer capability exactly when the operator enabled
/// direct peers and the worker advertised it. An unbooted coordinator has no
/// configuration that enables peers, as v2's optional `cfg` reads
/// (`deps.cfg?.terminalPeerEnabled === true`).
pub fn acknowledge_attachment_peer_capability(
    boot: &BootFacts,
    advertised: &[String],
    acknowledged: &mut Vec<String>,
) {
    let enabled = boot
        .config
        .as_deref()
        .is_some_and(|config| config.terminal_peer_enabled);
    let negotiated = enabled
        && advertised
            .iter()
            .any(|capability| capability == CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1);
    if negotiated {
        acknowledged.push(CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1.to_owned());
    }
    tracing::debug!(
        attachment_transfer_peer_webrtc_v1 = negotiated,
        "worker link: attachment peer capability decided"
    );
}

/// The typed attachment results one worker generation's end must fail.
#[derive(Debug)]
pub struct AttachmentWorkerLifecycle {
    peers: Arc<AttachmentPeerNegotiations>,
    statuses: Arc<AttachmentDirectStatusResults>,
}

impl AttachmentWorkerLifecycle {
    /// The observer over the process's two typed-result owners.
    #[must_use]
    pub fn new(
        peers: Arc<AttachmentPeerNegotiations>,
        statuses: Arc<AttachmentDirectStatusResults>,
    ) -> Self {
        Self { peers, statuses }
    }

    /// v2 `cancelAttachmentDirectWorkerResults`: both owners, same handle, same reason.
    fn cancel_attachment_direct_worker_results(&self, worker: &Arc<WorkerHandle>, reason: &str) {
        self.peers.cancel_for_worker_handle(worker, reason);
        self.statuses.cancel_for_worker_handle(worker, reason);
    }
}

impl WorkerLifecycleObserver for AttachmentWorkerLifecycle {
    fn on_superseded(&self, superseded: &Arc<WorkerHandle>) {
        self.cancel_attachment_direct_worker_results(superseded, "connection_superseded");
    }

    fn on_closed(&self, handle: &Arc<WorkerHandle>, end: LinkEnd) {
        let reason = match end {
            LinkEnd::Revoked => "worker_revoked",
            LinkEnd::Closed { .. } => "worker_disconnected",
        };
        self.cancel_attachment_direct_worker_results(handle, reason);
    }
}
