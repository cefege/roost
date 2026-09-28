//! The ports the coordinator link's downstream dispatch routes to, and the one
//! bundle of them the link loop holds. Called by `runtime::downstream` and the
//! link loop's lifecycle hooks; implemented by the terminal input, stream,
//! pipeline, view and cell owners, and composed in `runtime::owners`.
//! Ports the `CoordLinkDeps` callbacks and `CoordLinkPipelineState` of v2
//! `apps/worker/src/transport/coord-link-types.ts` / `coord-link-deps.ts`.

use std::sync::Arc;

use roost_proto::{
    DInputRequest, DTerminalInputRouteClaim, DTerminalPipelineSnapshotRequest,
    DTerminalStreamState, DTerminalViewRelay, TerminalInputRouteResult, WTerminalPipelineSnapshot,
};
use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::{
    InputResult, TerminalSnapshotRequest, TerminalStreamResult,
};

use crate::uplink::{LinkFence, OwnerFuture, RequestBudget};

/// Browser input reaching a PTY. v2 `onInputRequest`, `onBinary`,
/// `onTerminalInputRouteClaim`, and the route half of `onTerminalViewSocketClosed`.
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
    /// Legacy unacknowledged input (v2 `onBinary` → `sessionMgr.input`),
    /// already filtered to `DIR_TO_PTY`.
    fn write_binary(&self, channel_id: ChannelId, bytes: Vec<u8>);
    /// `None` → the dispatcher sends v2's refused claim (`route_claim_busy`).
    fn claim_route(
        &self,
        request: DTerminalInputRouteClaim,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Option<TerminalInputRouteResult>>;
    fn retire_connection(&self, socket_id: &str);
}

/// Coordinator-driven stream state and repair. v2 `onTerminalStreamState`,
/// `onTerminalSnapshotRequest`.
pub trait TerminalStreamPort: Send + Sync + std::fmt::Debug {
    /// `fence.is_current()` is v2's `budget.isCurrentConnection()`. `None` is a
    /// request no wire result can describe (a session id that is not a uuid):
    /// the dispatcher sends nothing.
    fn apply_stream_state(
        &self,
        request: DTerminalStreamState,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Option<TerminalStreamResult>>;
    fn request_snapshot(&self, request: TerminalSnapshotRequest);
}

/// Content-free pipeline evidence. v2 `onTerminalPipelineSnapshot`.
pub trait TerminalPipelinePort: Send + Sync + std::fmt::Debug {
    fn pipeline_snapshot(
        &self,
        request: DTerminalPipelineSnapshotRequest,
        link: LinkPipelineState,
    ) -> WTerminalPipelineSnapshot;
}

/// The link's own queue as pipeline evidence sees it. v2
/// `CoordLinkPipelineState`.
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
/// keep their receive order. v2 `onTerminalViewRelay`, the view half of
/// `onTerminalViewSocketClosed`, and `viewOwner.dropCoordinatorSockets`.
pub trait TerminalViewPort: Send + Sync + std::fmt::Debug {
    fn relay(&self, request: DTerminalViewRelay);
    fn close_socket(&self, socket_id: &str);
    fn drop_coordinator_sockets(&self);
}

/// The session half of the link's lifecycle. v2 `onOpen`, `onHelloAck`,
/// `onDetach`, `onWritable`, `onSnapshotReady`.
pub trait LinkLifecyclePort: Send + Sync + std::fmt::Debug {
    fn on_open(&self);
    fn on_hello_ack(&self, terminal_metadata_negotiated: bool);
    fn on_detach(&self);
    fn on_writable(&self);
    fn on_snapshot_ready(&self);
}

/// Every owner a downstream frame can route to.
#[derive(Clone, Debug)]
pub struct DownstreamOwners {
    pub input: Arc<dyn TerminalInputPort>,
    pub stream: Arc<dyn TerminalStreamPort>,
    pub pipeline: Arc<dyn TerminalPipelinePort>,
    pub view: Arc<dyn TerminalViewPort>,
    pub lifecycle: Arc<dyn LinkLifecyclePort>,
}
