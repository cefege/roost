//! The direct-attachment half of the local door, composed once: the direct
//! receiver, the attachment peer owner and the grant store they share, plus
//! the coordinator-link entry points for peer offers and cancels. Built by
//! `runtime::owners`; used by the door's attachment route, the attachment
//! grant-revoke arm, the peer offer/cancel arms and the direct-carrier detach.
//! Ports the attachment half of `apps/worker/src/boot/boot-local-terminal.ts`
//! and of `apps/worker/src/transport/coord-link-direct-deps.ts`.

use std::fmt;
use std::sync::Arc;

use roost_proto::{
    DLocalAttachmentPeerCancel, DLocalAttachmentPeerOffer, WLocalAttachmentPeerAnswer,
};
use roost_protocol::attachment_transfer::{PeerChannelLane, PeerErrorReason};

use super::direct_sockets::{AttachmentDirectSockets, AttachmentDirectSocketsDeps, DirectLane};
use super::grants::{AttachmentGrantStore, PeerGrantRequest};
use super::peer_budget::AttachmentPeerPacketBudget;
use super::peer_connection::OpenAttachmentPeerPort;
use super::peer_owner::{
    AttachmentPeerBootstrapState, AttachmentPeerOwner, AttachmentPeerOwnerDeps,
};
use super::peer_packet_port::{AttachmentPeerIngress, AttachmentPeerPacketPort};
use super::transfer_admission::AttachmentPeerExpectedTuple;
use super::transfer_port::AttachmentTransferPort;
use super::upload::AttachmentOperations;
use crate::link_ports::AttachmentPeerPort;
use crate::peer::PeerTransportConfig;
use crate::peer::coordinator_generation::CoordinatorGeneration;
use crate::peer::direct::DirectCarrier;
use crate::peer::native::NativeLoader;
use crate::uplink::{LinkFence, OwnerFuture, RequestBudget};

pub struct AttachmentDirectDeps {
    pub grants: Arc<AttachmentGrantStore>,
    pub operations: AttachmentOperations,
    pub worker_fingerprint: String,
    pub worker_epoch: String,
    pub peer: PeerTransportConfig,
    pub native_loader: NativeLoader,
    pub coordinator_generation: CoordinatorGeneration,
}

impl fmt::Debug for AttachmentDirectDeps {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentDirectDeps")
            .field("worker_epoch", &self.worker_epoch)
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

/// Clone shares one set of owners.
#[derive(Clone, Debug)]
pub struct AttachmentDirect {
    sockets: AttachmentDirectSockets,
    peer_owner: AttachmentPeerOwner,
    grants: Arc<AttachmentGrantStore>,
    coordinator_generation: CoordinatorGeneration,
}

impl AttachmentDirect {
    pub fn new(deps: AttachmentDirectDeps) -> Self {
        let sockets = AttachmentDirectSockets::new(AttachmentDirectSocketsDeps {
            grants: Arc::clone(&deps.grants),
            operations: deps.operations,
            worker_fingerprint: deps.worker_fingerprint,
            worker_epoch: deps.worker_epoch.clone(),
        });
        let generation = deps.coordinator_generation.clone();
        let grants = Arc::clone(&deps.grants);
        let peer_owner = AttachmentPeerOwner::new(AttachmentPeerOwnerDeps {
            process_epoch: deps.worker_epoch,
            enabled: deps.peer.enabled,
            bind_address: deps.peer.bind_address,
            port_range: deps.peer.port_range,
            is_current_coordinator: Arc::new(move |candidate: &str| {
                generation.is_current(candidate)
            }),
            authorize_grant: Arc::new(move |request: &DLocalAttachmentPeerOffer| {
                grants.authorize_peer(&PeerGrantRequest {
                    grant_id: request.grant_id.clone(),
                    device_fingerprint: request.device_fingerprint.clone(),
                    tab_id: request.tab_id.clone(),
                    worker_epoch: request.worker_epoch.clone(),
                })
            }),
            open_peer_port: open_peer_port(sockets.clone()),
            native_loader: deps.native_loader,
            packet_budget: AttachmentPeerPacketBudget::new(),
        });
        Self {
            sockets,
            peer_owner,
            grants: deps.grants,
            coordinator_generation: deps.coordinator_generation,
        }
    }

    /// The receiver the door's attachment route drives.
    pub fn sockets(&self) -> AttachmentDirectSockets {
        self.sockets.clone()
    }

    /// Advertised as the attachment peer capability only when `Ready`.
    pub async fn bootstrap(&self) -> AttachmentPeerBootstrapState {
        self.peer_owner.bootstrap().await
    }

    /// First of v2 `revokeAttachmentDevice`: admitted ports, even past expiry.
    pub fn revoke_sockets(&self, device_fingerprint: &str) {
        self.sockets.revoke_device(device_fingerprint);
    }

    /// Last of v2 `revokeAttachmentDevice`: pending and established peers.
    pub fn revoke_peers(&self, device_fingerprint: &str) {
        self.peer_owner.revoke_device(device_fingerprint);
    }
}

impl AttachmentPeerPort for AttachmentDirect {
    /// A generation this link did not accept first supersedes the offer; the
    /// owner's admission runs before this returns.
    fn offer(
        &self,
        request: DLocalAttachmentPeerOffer,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Result<WLocalAttachmentPeerAnswer, PeerErrorReason>> {
        if !self
            .coordinator_generation
            .use_generation(&request.connection_generation)
        {
            return Box::pin(async { Err(PeerErrorReason::ConnectionSuperseded) });
        }
        self.peer_owner.offer(request, budget, fence)
    }

    fn cancel(&self, request: &DLocalAttachmentPeerCancel) {
        if self
            .coordinator_generation
            .use_generation(&request.connection_generation)
        {
            self.peer_owner.cancel(request);
        }
    }
}

impl DirectCarrier for AttachmentDirect {
    fn cancel_pending_for_coordinator(&self) {
        self.peer_owner.cancel_pending_for_coordinator();
    }

    /// v2 `disposeDirect` order: sockets, peers, then the grants they read.
    fn dispose(&self) {
        self.sockets.dispose();
        self.peer_owner.dispose();
        self.grants.dispose();
    }
}

/// A peer port opens as a direct receiver session bound to its expected
/// tuple; its reassembled frames arrive on their own lane.
fn open_peer_port(sockets: AttachmentDirectSockets) -> OpenAttachmentPeerPort {
    Arc::new(
        move |port: Arc<AttachmentPeerPacketPort>, expected_tuple: AttachmentPeerExpectedTuple| {
            let port: Arc<dyn AttachmentTransferPort> = port;
            let socket_id = port.socket_id().to_owned();
            if !sockets.open_peer_port(port, expected_tuple) {
                return None;
            }
            let ingress: Arc<dyn AttachmentPeerIngress> = Arc::new(DirectPeerIngress {
                sockets: sockets.clone(),
                socket_id,
            });
            Some(ingress)
        },
    )
}

#[derive(Debug)]
struct DirectPeerIngress {
    sockets: AttachmentDirectSockets,
    socket_id: String,
}

impl AttachmentPeerIngress for DirectPeerIngress {
    fn on_message(&self, lane: PeerChannelLane, bytes: Vec<u8>) {
        let write = self
            .sockets
            .receive_frame(&self.socket_id, DirectLane::Peer(lane), &bytes);
        if let Some(write) = write {
            tokio::spawn(write);
        }
    }

    fn on_close(&self) {
        self.sockets.close_port(&self.socket_id);
    }
}
