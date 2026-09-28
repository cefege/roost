//! The bounded route-control requests and the typed worker-result validation
//! that the Sync admission and the result owner share, so the two can never
//! disagree about a limit. Pure: nothing here sends a frame or holds state.
//! Read by `terminal_input::route_results` and `terminal_input::sync_route_controls`.
//! Ports `apps/coord/src/terminal/input/terminal-input-route-result-contract.ts`.

use std::sync::Arc;

use connectrpc::ConnectError;
use roost_proto::{WTerminalInputRouteResult, WTerminalTransportProbeResult};
use roost_protocol::terminal_peer::peer::TERMINAL_PEER_ROUTE_CLAIM_MAX_OUTSTANDING;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::terminal_input::route_state::PendingRouteControl;
use crate::workers::hop_deadline::HopDeadline;

/// Route controls one Sync socket may hold open at once.
pub const MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET: usize = TERMINAL_PEER_ROUTE_CLAIM_MAX_OUTSTANDING;

/// The byte bound on every opaque route identifier: request ids, epochs,
/// session ids, the socket id, and a result's reason.
pub const TERMINAL_ROUTE_IDENTIFIER_MAX_UTF8_BYTES: usize = 128;

/// The largest route revision the worker's signed counter can carry.
pub const MAX_TERMINAL_INPUT_ROUTE_REVISION: u64 = (1 << 63) - 1;

/// A definite refusal: no route-control frame reached the worker, so the
/// browser may treat it as "not here" rather than as a lost answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteControlRefusal {
    /// The socket already holds its bound of controls, or this nonce.
    RouteClaimBusy,
    /// The claim cannot be routed to a route-capable worker generation.
    InputRouteUnavailable,
    /// The probe cannot be routed to a route-capable worker generation.
    TransportProbeUnavailable,
}

impl RouteControlRefusal {
    /// v2's reason spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RouteClaimBusy => "route_claim_busy",
            Self::InputRouteUnavailable => "terminal_input_route_unavailable",
            Self::TransportProbeUnavailable => "terminal_transport_probe_unavailable",
        }
    }
}

/// How a claim or probe failed.
///
/// The two are different KINDS of answer, and the Sync admission treats them
/// differently: a refusal is known to have sent nothing and is answered at
/// once, while a failure after the worker frame went out (a deadline, a
/// cancellation, an invalid result) cannot be answered truthfully and is not.
#[derive(Debug, Clone)]
pub enum RouteControlError {
    /// Nothing reached the worker.
    Refused(RouteControlRefusal),
    /// The worker frame may have been sent; the outcome is unknown.
    Failed(ConnectError),
}

impl From<RouteControlRefusal> for RouteControlError {
    fn from(refusal: RouteControlRefusal) -> Self {
        Self::Refused(refusal)
    }
}

/// One browser's claim of a session's input route, bound to the exact worker
/// generation its admission resolved.
#[derive(Debug, Clone)]
pub struct InputRouteClaimRequest {
    /// The browser's own nonce, restored on the typed result.
    pub browser_request_id: String,
    /// The session whose route is claimed.
    pub session_id: String,
    /// The browser's claim revision.
    pub revision: u64,
    /// The authenticated device.
    pub device_fingerprint: String,
    /// The authenticated tab.
    pub tab_id: String,
    /// The Sync socket that owns the claim.
    pub connection_id: String,
    /// The worker generation the claim must reach.
    pub worker: Arc<WorkerHandle>,
    /// The worker process epoch the browser named.
    pub worker_epoch: String,
    /// The hop budget; production starts it at entry.
    pub deadline: HopDeadline,
}

/// One browser's content-free probe of a worker's transport.
#[derive(Debug, Clone)]
pub struct TransportProbeRequest {
    /// The browser's own nonce, restored on the typed result.
    pub browser_request_id: String,
    /// The Sync socket that owns the probe.
    pub connection_id: String,
    /// The worker the browser named.
    pub worker_fp: String,
    /// The worker generation the probe must reach.
    pub worker: Arc<WorkerHandle>,
    /// The worker process epoch the probe expects back.
    pub worker_epoch: String,
    /// The hop budget; production starts it at entry.
    pub deadline: HopDeadline,
}

/// Whether a value is an opaque route identifier: non-empty and bounded.
#[must_use]
pub fn is_terminal_route_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= TERMINAL_ROUTE_IDENTIFIER_MAX_UTF8_BYTES
}

/// Whether a worker's route result answers exactly this pending claim.
#[must_use]
pub fn is_valid_input_route_result(
    pending: &PendingRouteControl,
    frame: &WTerminalInputRouteResult,
) -> bool {
    let (Some(result), Some(outer)) = (frame.result.as_option(), &pending.outer_request_id) else {
        return false;
    };
    let answers_this_claim = frame.request_id == *outer
        && result.request_id == *outer
        && pending.session_id.as_deref() == Some(result.session_id.as_str())
        && pending.revision == Some(result.revision)
        && result.worker_epoch == pending.worker_epoch
        && result.latest_revision <= MAX_TERMINAL_INPUT_ROUTE_REVISION
        && result.reason.len() <= TERMINAL_ROUTE_IDENTIFIER_MAX_UTF8_BYTES;
    if !answers_this_claim {
        return false;
    }
    if result.accepted {
        is_terminal_route_identifier(&result.input_route_epoch)
    } else {
        result.input_route_epoch.is_empty()
    }
}

/// Whether a worker's probe result answers exactly this pending probe.
#[must_use]
pub fn is_valid_transport_probe_result(
    pending: &PendingRouteControl,
    frame: &WTerminalTransportProbeResult,
) -> bool {
    pending.outer_request_id.as_deref() == Some(frame.request_id.as_str())
        && frame.worker_epoch == pending.worker_epoch
}
