//! The direct terminal path as the coordinator link reaches it: the terminal
//! peer owner built over the local door's sockets and grants, the coordinator
//! generation both peer owners share, and the peer offer, cancel, transport
//! probe, direct retire, device revoke and link-detach controls. Built by
//! `runtime::owners`; called by `runtime::downstream::direct` and the link
//! lifecycle. Ports v2 `apps/worker/src/transport/coord-link-direct-deps.ts`
//! and the peer half of `apps/worker/src/boot/boot-local-terminal.ts`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use roost_proto::{
    DLocalTerminalGrant, DLocalTerminalPeerCancel, DLocalTerminalPeerOffer, DTerminalDirectRetire,
    DTerminalTransportProbe, WLocalTerminalPeerAnswer, WTerminalTransportProbeResult,
};
use tokio::runtime::Handle;

use super::config::PeerTransportConfig;
use super::connection::OpenTerminalPeerPort;
use super::coordinator_generation::CoordinatorGeneration;
use super::faults::PeerTestFaults;
use super::native::NativeLoader;
use super::owner::{TerminalPeerOfferFailure, TerminalPeerOwner, TerminalPeerOwnerDeps};
use super::packet_budget::{TerminalPeerPacketBudget, lock};
use super::packet_port::{TerminalPeerPacketIngress, TerminalPeerPacketPort};
use crate::link_ports::{DirectTerminalPort, LinkLifecyclePort, LocalTerminalGrantPort};
use crate::local_terminal::{
    ExpectedPeer, GrantRemovalReason, LocalTerminalDoor, PeerTerminalPacketPort,
};
use crate::uplink::{LinkFence, OwnerFuture, RequestBudget};

/// v2's only retire reasons (`coord-link-direct-deps.ts` `onTerminalDirectRetire`).
const RETIRE_REASONS: [&str; 2] = ["worker_deleted", "worker_revoked"];

/// Another direct carrier retired with the terminal one: the attachment peer
/// bundle (v2 `attachmentPeerOwner`, `attachmentSockets`, `attachmentGrants`).
pub trait DirectCarrier: Send + Sync + std::fmt::Debug {
    /// v2 `cancelPendingForCoordinator` on link detach.
    fn cancel_pending_for_coordinator(&self);
    /// v2 `disposeDirect`.
    fn dispose(&self);
}

/// What the hello may advertise about the direct carriers (v2 main.ts
/// `peerSupported` / `attachmentPeerSupported`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirectPeerSupport {
    pub terminal: bool,
    pub attachment: bool,
}

/// What [`DirectTerminal`] is built over.
pub struct DirectTerminalDeps {
    pub door: Arc<LocalTerminalDoor>,
    pub process_epoch: String,
    pub transport: PeerTransportConfig,
    pub native_loader: NativeLoader,
    /// Smoke-only; `None` for every ordinary worker.
    pub test_faults: Option<Arc<PeerTestFaults>>,
    pub runtime: Handle,
}

impl std::fmt::Debug for DirectTerminalDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DirectTerminalDeps")
            .field("transport", &self.transport)
            .finish_non_exhaustive()
    }
}

/// v2 `LocalTerminalWiring`'s peer half plus `makeCoordLinkDirectTerminalHandlers`.
#[derive(Debug)]
pub struct DirectTerminal {
    door: Arc<LocalTerminalDoor>,
    peer_owner: Arc<TerminalPeerOwner>,
    generation: CoordinatorGeneration,
    carriers: Mutex<Vec<Arc<dyn DirectCarrier>>>,
    process_epoch: String,
    disposed: AtomicBool,
    /// Smoke-only; `None` for every ordinary worker.
    test_faults: Option<Arc<PeerTestFaults>>,
}

impl DirectTerminal {
    pub fn new(deps: DirectTerminalDeps) -> Arc<Self> {
        let generation = CoordinatorGeneration::new();
        let current = generation.clone();
        let grants = deps.door.grants();
        let expiring = grants.clone();
        let sockets = deps.door.sockets();
        let open_peer_port: OpenTerminalPeerPort = Arc::new(
            move |port: Arc<TerminalPeerPacketPort>, expected: ExpectedPeer| {
                let port: Arc<dyn PeerTerminalPacketPort> = port;
                let ingress: Arc<dyn TerminalPeerPacketIngress> =
                    Arc::new(sockets.open_peer_port(port, expected));
                Some(ingress)
            },
        );
        let peer_owner = TerminalPeerOwner::new(TerminalPeerOwnerDeps {
            process_epoch: deps.process_epoch.clone(),
            transport: deps.transport,
            is_current_coordinator: Arc::new(move |generation: &str| {
                current.is_current(generation)
            }),
            authorize_grant: Arc::new(move |request: &DLocalTerminalPeerOffer| {
                grants.authorize_peer(
                    &request.grant_id,
                    &request.device_fingerprint,
                    &request.tab_id,
                    &request.worker_epoch,
                )
            }),
            open_peer_port,
            native_loader: deps.native_loader,
            packet_budget: TerminalPeerPacketBudget::new(),
            test_faults: deps.test_faults.clone(),
            expire_grant: Arc::new(move |grant_id: &str| {
                expiring.remove(grant_id, GrantRemovalReason::Expired);
            }),
            runtime: deps.runtime,
        });
        Arc::new(Self {
            door: deps.door,
            peer_owner,
            generation,
            carriers: Mutex::new(Vec::new()),
            process_epoch: deps.process_epoch,
            disposed: AtomicBool::new(false),
            test_faults: deps.test_faults,
        })
    }

    pub fn peer_owner(&self) -> &Arc<TerminalPeerOwner> {
        &self.peer_owner
    }

    /// The generation gate the attachment peer owner shares.
    pub fn generation(&self) -> CoordinatorGeneration {
        self.generation.clone()
    }

    pub fn register_carrier(&self, carrier: Arc<dyn DirectCarrier>) {
        lock(&self.carriers).push(carrier);
    }

    /// v2 `onDetach`'s direct half: only unsettled signaling is fenced by
    /// coordinator loss; established peers keep their grant lifetime.
    pub fn coordinator_detached(&self) {
        self.generation.clear();
        self.peer_owner.cancel_pending_for_coordinator();
        for carrier in self.carriers() {
            carrier.cancel_pending_for_coordinator();
        }
    }

    /// v2 `disposeDirect`, once: stop every direct carrier, grant and route.
    pub fn dispose_direct(&self) {
        if self.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.generation.clear();
        self.door.dispose();
        for carrier in self.carriers() {
            carrier.dispose();
        }
        self.peer_owner.dispose();
        tracing::info!("the direct terminal carriers were disposed");
    }

    fn carriers(&self) -> Vec<Arc<dyn DirectCarrier>> {
        lock(&self.carriers).clone()
    }
}

impl DirectTerminalPort for DirectTerminal {
    /// v2 `onLocalTerminalPeerOffer`: the offer's generation must be the one
    /// this link adopted.
    fn peer_offer(
        &self,
        request: DLocalTerminalPeerOffer,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Result<WLocalTerminalPeerAnswer, TerminalPeerOfferFailure>> {
        if !self
            .generation
            .use_generation(&request.connection_generation)
        {
            return Box::pin(async { Err(TerminalPeerOfferFailure::ConnectionSuperseded) });
        }
        self.peer_owner.offer(request, budget, fence)
    }

    fn peer_cancel(&self, request: &DLocalTerminalPeerCancel) {
        if self
            .generation
            .use_generation(&request.connection_generation)
        {
            self.peer_owner.cancel(request);
        }
    }

    /// v2 `onTerminalTransportProbe`: answered for this process's epoch only.
    fn transport_probe(
        &self,
        request: &DTerminalTransportProbe,
    ) -> Option<WTerminalTransportProbeResult> {
        (request.worker_epoch == self.process_epoch).then(|| WTerminalTransportProbeResult {
            request_id: request.request_id.clone(),
            worker_epoch: self.process_epoch.clone(),
            ..Default::default()
        })
    }

    /// v2 `onTerminalDirectRetire`: a deleted or revoked worker retires every
    /// direct carrier.
    fn direct_retire(&self, request: &DTerminalDirectRetire) {
        if request.worker_epoch != self.process_epoch
            || !RETIRE_REASONS.contains(&request.reason.as_str())
        {
            return;
        }
        if self
            .test_faults
            .as_ref()
            .is_some_and(|faults| faults.consume_direct_retire_drop())
        {
            tracing::info!(reason = %request.reason, "a direct retirement was ignored by the smoke harness");
            return;
        }
        tracing::info!(reason = %request.reason, "the direct terminal path was retired");
        self.dispose_direct();
    }
}

impl LocalTerminalGrantPort for DirectTerminal {
    fn install_grant(&self, request: &DLocalTerminalGrant) -> Result<(), String> {
        self.door.install_grant(request)
    }

    /// v2 `wiring.revokeDevice`: routes, grants, then this device's peers.
    fn revoke_device(&self, device_fingerprint: &str) {
        self.door.revoke_device(device_fingerprint);
        self.peer_owner.revoke_device(device_fingerprint);
    }
}

/// The link lifecycle with the direct half of v2 `onDetach` in front of the
/// session half, in v2's order. Wraps the session lifecycle in `runtime::owners`.
#[derive(Debug)]
pub struct DirectLinkLifecycle {
    direct: Arc<DirectTerminal>,
    sessions: Arc<dyn LinkLifecyclePort>,
}

impl DirectLinkLifecycle {
    pub fn new(direct: Arc<DirectTerminal>, sessions: Arc<dyn LinkLifecyclePort>) -> Self {
        Self { direct, sessions }
    }
}

impl LinkLifecyclePort for DirectLinkLifecycle {
    fn on_open(&self) {
        self.sessions.on_open();
    }

    fn on_hello_ack(&self, terminal_metadata_negotiated: bool) {
        self.sessions.on_hello_ack(terminal_metadata_negotiated);
    }

    fn on_detach(&self) {
        self.direct.coordinator_detached();
        self.sessions.on_detach();
    }

    fn on_writable(&self) {
        self.sessions.on_writable();
    }

    fn on_snapshot_ready(&self) {
        self.sessions.on_snapshot_ready();
    }
}
