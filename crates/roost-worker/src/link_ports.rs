//! The ports the coordinator link's downstream dispatch routes to, and the one
//! bundle of them the link loop holds. Called by `runtime::downstream` and the
//! link loop's lifecycle hooks; implemented by the terminal input, stream,
//! pipeline, view and cell owners, and composed in `runtime::owners`.

use std::sync::Arc;

use roost_proto::DKeeperUpdatePrepare;
use roost_proto::{
    AttachmentTransferStatus, DAttachmentChunk, DAttachmentDirectStatusRequest,
    DLocalAttachmentGrant,
};
use roost_proto::{
    DInputRequest, DLocalTerminalGrant, DTerminalInputRouteClaim, DTerminalPipelineSnapshotRequest,
    DTerminalStreamState, DTerminalViewRelay, TerminalInputRouteResult, WTerminalPipelineSnapshot,
};
use roost_proto::{
    DLocalAttachmentPeerCancel, DLocalAttachmentPeerOffer, WLocalAttachmentPeerAnswer,
};
use roost_proto::{
    DLocalTerminalPeerCancel, DLocalTerminalPeerOffer, DTerminalDirectRetire,
    DTerminalTransportProbe, WLocalTerminalPeerAnswer, WTerminalTransportProbeResult,
};
use roost_protocol::attachment_transfer::PeerErrorReason;
use roost_protocol::wire::coord_worker::{
    InputResult, TerminalSnapshotRequest, TerminalStreamResult,
};

use crate::attachments::upload::RelayChunkOutcome;
use crate::peer::TerminalPeerOfferFailure;
use crate::uplink::{LinkFence, OwnerFuture, RequestBudget};

/// Browser input reaching a PTY: `onInputRequest`, `onTerminalInputRouteClaim`,
/// and the route half of `onTerminalViewSocketClosed`.
pub trait TerminalInputPort: Send + Sync + std::fmt::Debug {
    /// Work-budget reservation happens synchronously in this call; the future
    /// resolves to the ONE `input-result` payload the coordinator is told, or
    /// `None` when no wire result can describe the request (a session id that
    /// is not a uuid) and the dispatcher sends nothing.
    fn write_input(
        &self,
        request: DInputRequest,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Option<InputResult>>;
    /// `None` → the dispatcher sends the refused claim (`route_claim_busy`).
    fn claim_route(
        &self,
        request: DTerminalInputRouteClaim,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Option<TerminalInputRouteResult>>;
    fn retire_connection(&self, socket_id: &str);
}

/// Coordinator-driven stream state and repair.
pub trait TerminalStreamPort: Send + Sync + std::fmt::Debug {
    /// `fence.is_current()` reports whether the request's connection is still
    /// the live one. `None` is a request no wire result can describe (a
    /// session id that is not a uuid): the dispatcher sends nothing.
    fn apply_stream_state(
        &self,
        request: DTerminalStreamState,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Option<TerminalStreamResult>>;
    fn request_snapshot(&self, request: TerminalSnapshotRequest);
}

/// Content-free pipeline evidence.
pub trait TerminalPipelinePort: Send + Sync + std::fmt::Debug {
    fn pipeline_snapshot(
        &self,
        request: DTerminalPipelineSnapshotRequest,
        link: LinkPipelineState,
    ) -> WTerminalPipelineSnapshot;
}

/// The link's own queue as pipeline evidence sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkPipelineState {
    pub queue_frames: u64,
    pub queue_bytes: u64,
    /// Bytes the socket buffered below the outbox. Zero: the writer awaits
    /// every send, so nothing is buffered that this process can measure.
    pub native_buffered_bytes: u64,
    pub attached: bool,
}

/// Worker-owned terminal views. Synchronous, so one browser socket's decisions
/// keep their receive order.
pub trait TerminalViewPort: Send + Sync + std::fmt::Debug {
    fn relay(&self, request: DTerminalViewRelay);
    fn close_socket(&self, socket_id: &str);
    fn drop_coordinator_sockets(&self);
}

/// The session half of the link's lifecycle.
pub trait LinkLifecyclePort: Send + Sync + std::fmt::Debug {
    fn on_open(&self);
    fn on_hello_ack(&self, capabilities: &[String]);
    fn on_detach(&self);
    fn on_writable(&self);
    fn on_snapshot_ready(&self);
}

/// Several owners of the link's lifecycle behind the one port the link calls.
/// Each callback reaches several owners: `on_snapshot_ready` replays terminal
/// metadata and resumes the cell sink in registration order.
#[derive(Debug)]
pub struct LinkLifecycles {
    owners: Vec<Arc<dyn LinkLifecyclePort>>,
}

impl LinkLifecycles {
    pub fn new(owners: Vec<Arc<dyn LinkLifecyclePort>>) -> Self {
        Self { owners }
    }
}

impl LinkLifecyclePort for LinkLifecycles {
    fn on_open(&self) {
        self.owners.iter().for_each(|owner| owner.on_open());
    }
    fn on_hello_ack(&self, capabilities: &[String]) {
        self.owners
            .iter()
            .for_each(|owner| owner.on_hello_ack(capabilities));
    }
    fn on_detach(&self) {
        self.owners.iter().for_each(|owner| owner.on_detach());
    }
    fn on_writable(&self) {
        self.owners.iter().for_each(|owner| owner.on_writable());
    }
    fn on_snapshot_ready(&self) {
        self.owners
            .iter()
            .for_each(|owner| owner.on_snapshot_ready());
    }
}

/// The local terminal door's grant half. Implemented by `local_terminal::LocalTerminalDoor`.
pub trait LocalTerminalGrantPort: Send + Sync + std::fmt::Debug {
    /// Install or renew a grant; `Err` is the message the install is refused with.
    fn install_grant(&self, request: &DLocalTerminalGrant) -> Result<(), String>;
    /// Fence a device: its input routes, then its grants (which close its sockets).
    fn revoke_device(&self, device_fingerprint: &str);
}

/// The direct terminal path. Implemented by `peer::DirectTerminal`.
pub trait DirectTerminalPort: Send + Sync + std::fmt::Debug {
    /// Negotiate one browser peer; the answer or refusal is fenced to `fence`.
    fn peer_offer(
        &self,
        request: DLocalTerminalPeerOffer,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Result<WLocalTerminalPeerAnswer, TerminalPeerOfferFailure>>;
    fn peer_cancel(&self, request: &DLocalTerminalPeerCancel);
    /// `None` when the probe is not for this worker's direct path.
    fn transport_probe(
        &self,
        request: &DTerminalTransportProbe,
    ) -> Option<WTerminalTransportProbeResult>;
    fn direct_retire(&self, request: &DTerminalDirectRetire);
}

/// Attachment peers. Implemented by
/// `attachments::direct_owners::AttachmentDirect`.
pub trait AttachmentPeerPort: Send + Sync + std::fmt::Debug {
    /// Admission and the pending reservation happen synchronously in this
    /// call, so a cancel dispatched right after it finds the offer.
    fn offer(
        &self,
        request: DLocalAttachmentPeerOffer,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Result<WLocalAttachmentPeerAnswer, PeerErrorReason>>;
    fn cancel(&self, request: &DLocalAttachmentPeerCancel);
}

/// Relayed attachment uploads and the direct-carrier grants the coordinator
/// installs. Implemented by `attachments::link::AttachmentLink`.
pub trait AttachmentLinkPort: Send + Sync + std::fmt::Debug {
    /// The chunk is written in this call (arrival order is write order); the
    /// future settles what the coordinator is told.
    fn accept_relay_chunk(&self, chunk: DAttachmentChunk) -> OwnerFuture<RelayChunkOutcome>;
    /// `Err` is the message the `rpc-error` carries.
    fn install_grant(&self, request: &DLocalAttachmentGrant) -> Result<(), String>;
    fn revoke_device(&self, device_fingerprint: &str);
    fn direct_status(&self, request: &DAttachmentDirectStatusRequest) -> AttachmentTransferStatus;
}

/// Keeper replacement preparation. Implemented by
/// `keeper_pool::KeeperUpdatePreparer`.
pub trait KeeperUpdatePort: Send + Sync + std::fmt::Debug {
    /// Channel admission closes synchronously in this call, before the
    /// preparation joins the serialized queue; the future resolves to the
    /// `rpc-ok` data, or the `rpc-error` message.
    fn prepare(
        &self,
        request: DKeeperUpdatePrepare,
    ) -> OwnerFuture<Result<serde_json::Value, String>>;
}

/// Coordinator-driven live host metrics: a subscription starts or stops sampling.
pub trait LiveMetricsPort: Send + Sync + std::fmt::Debug {
    fn subscription(&self, request: roost_proto::DLiveMetricsSubscription);
}

/// Every owner a downstream frame can route to.
#[derive(Clone, Debug)]
pub struct DownstreamOwners {
    pub input: Arc<dyn TerminalInputPort>,
    pub stream: Arc<dyn TerminalStreamPort>,
    pub pipeline: Arc<dyn TerminalPipelinePort>,
    pub view: Arc<dyn TerminalViewPort>,
    pub lifecycle: Arc<dyn LinkLifecyclePort>,
    pub local_terminal: Arc<dyn LocalTerminalGrantPort>,
    pub direct: Option<Arc<dyn DirectTerminalPort>>,
    pub attachment_peers: Option<Arc<dyn AttachmentPeerPort>>,
    pub attachments: Arc<dyn AttachmentLinkPort>,
    pub keeper_update: Arc<dyn KeeperUpdatePort>,
    pub live_metrics: Option<Arc<dyn LiveMetricsPort>>,
}
