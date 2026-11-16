//! The direct terminal path as the coordinator link reaches it: the terminal
//! peer owner built over the local door's sockets and grants, the coordinator
//! generation both peer owners share, and the peer offer, cancel, transport
//! probe, direct retire, device revoke and link-detach controls. Built by
//! `runtime::owners`; called by `runtime::downstream::direct` and the link
//! lifecycle.

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
use super::native::NativeLoader;
use super::owner::{TerminalPeerOfferFailure, TerminalPeerOwner, TerminalPeerOwnerDeps};
use super::packet_budget::{TerminalPeerPacketBudget, lock};
use super::packet_port::{TerminalPeerPacketIngress, TerminalPeerPacketPort};
use crate::link_ports::{DirectTerminalPort, LinkLifecyclePort, LocalTerminalGrantPort};
use crate::local_terminal::{ExpectedPeer, LocalTerminalDoor, PeerTerminalPacketPort};
use crate::uplink::{LinkFence, OwnerFuture, RequestBudget};

/// The only retire reasons.
const RETIRE_REASONS: [&str; 2] = ["worker_deleted", "worker_revoked"];

/// Another direct carrier retired with the terminal one: the attachment peer
/// owner, its sockets and its grants.
pub trait DirectCarrier: Send + Sync + std::fmt::Debug {
    /// Cancel pending signaling on link detach.
    fn cancel_pending_for_coordinator(&self);
    /// Dispose the carrier and the grants it reads.
    fn dispose(&self);
}

/// What the hello may advertise about the direct carriers.
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

/// The peer half of the local terminal wiring and its coordinator-link handlers.
#[derive(Debug)]
pub struct DirectTerminal {
    door: Arc<LocalTerminalDoor>,
    peer_owner: Arc<TerminalPeerOwner>,
    generation: CoordinatorGeneration,
    carriers: Mutex<Vec<Arc<dyn DirectCarrier>>>,
    process_epoch: String,
    disposed: AtomicBool,
}

impl DirectTerminal {
    pub fn new(deps: DirectTerminalDeps) -> Arc<Self> {
        let generation = CoordinatorGeneration::new();
        let current = generation.clone();
        let grants = deps.door.grants();
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
            runtime: deps.runtime,
        });
        Arc::new(Self {
            door: deps.door,
            peer_owner,
            generation,
            carriers: Mutex::new(Vec::new()),
            process_epoch: deps.process_epoch,
            disposed: AtomicBool::new(false),
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

    /// The direct half of link detach: only unsettled signaling is fenced by
    /// coordinator loss; established peers keep their grant lifetime.
    pub fn coordinator_detached(&self) {
        self.generation.clear();
        self.peer_owner.cancel_pending_for_coordinator();
        for carrier in self.carriers() {
            carrier.cancel_pending_for_coordinator();
        }
    }

    /// Dispose the direct side, once: stop every direct carrier, grant and route.
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
    /// The offer's generation must be the one this link adopted.
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

    /// Answered for this process's epoch only.
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

    /// A deleted or revoked worker retires every direct carrier.
    fn direct_retire(&self, request: &DTerminalDirectRetire) {
        if request.worker_epoch != self.process_epoch
            || !RETIRE_REASONS.contains(&request.reason.as_str())
        {
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

    /// Routes, grants, then this device's peers.
    fn revoke_device(&self, device_fingerprint: &str) {
        self.door.revoke_device(device_fingerprint);
        self.peer_owner.revoke_device(device_fingerprint);
    }
}

/// The link lifecycle with the direct half of detach in front of the session
/// half. Wraps the session lifecycle in `runtime::owners`.
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

    fn on_hello_ack(&self, capabilities: &[String]) {
        self.sessions.on_hello_ack(capabilities);
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
