//! The worker frames of input-route control: one route claim, one empty
//! transport probe, and the notice that a Sync socket's routes are retired,
//! each to one exact worker generation. Correlation and browser replies belong
//! to `terminal_input::route_results`; this module only allocates the
//! coordinator's request id and writes the frame, on the terminal hop budget.
//! Ports `apps/coord/src/terminal/input/worker-send-terminal-route.ts`.

use std::sync::Arc;

use roost_proto::{
    DTerminalInputRouteClaim, DTerminalTransportProbe, DTerminalViewSocketClosed,
    WTerminalInputRouteResult, WTerminalTransportProbeResult,
};
use roost_protocol::versioning::CAPABILITY_TERMINAL_INPUT_ROUTE_V1;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::terminal_screen::pending_rpcs::PendingRpcs;
use crate::terminal_screen::typed_results::TypedResult;
use crate::workers::hop_deadline::{HopDeadline, worker_budget_ms};
use crate::workers::terminal_request::TerminalWorkerRequest;

/// The whole wait for one route claim or probe result.
pub const TERMINAL_ROUTE_CONTROL_TIMEOUT_MS: u64 = 8_000;

/// The actor and revision a route claim carries to the worker.
#[derive(Debug, Clone)]
pub struct InputRouteClaimSend {
    /// The session whose route is claimed.
    pub session_id: String,
    /// The authenticated device.
    pub device_fingerprint: String,
    /// The authenticated tab.
    pub tab_id: String,
    /// The Sync socket that owns the claim.
    pub browser_connection_id: String,
    /// The browser's claim revision.
    pub revision: u64,
    /// The worker process epoch the claim addresses.
    pub worker_epoch: String,
}

/// The two hooks the result owner supplies around a send.
///
/// `install` records the coordinator's request id BEFORE the frame goes out,
/// so a worker result that races the send still finds its correlation record;
/// `still_pending` re-checks, after that record exists and before the write,
/// that the owner has not cancelled the control meanwhile.
pub(crate) struct RouteCorrelation<Install, Verify> {
    /// Record the allocated request id, or say why the control is gone.
    pub(crate) install: Install,
    /// Whether the control is still the owner's to send.
    pub(crate) still_pending: Verify,
}

/// True only while this exact handle is the fingerprint's route-capable,
/// ready, unfenced generation at `expected_epoch`.
#[must_use]
pub fn is_current_terminal_input_route_worker(
    workers: &WorkerRegistry,
    worker: &Arc<WorkerHandle>,
    expected_epoch: &str,
) -> bool {
    worker.is_routable()
        && worker.process_epoch.as_deref() == Some(expected_epoch)
        && worker
            .capabilities
            .contains(CAPABILITY_TERMINAL_INPUT_ROUTE_V1)
        && workers
            .current(&worker.worker_fp)
            .is_some_and(|current| Arc::ptr_eq(&current, worker))
}

/// Allocate the coordinator's request id, then send one route claim.
pub(crate) fn send_terminal_input_route_claim_request<Install, Verify>(
    workers: &WorkerRegistry,
    pending_rpcs: &Arc<PendingRpcs>,
    worker: &Arc<WorkerHandle>,
    message: &InputRouteClaimSend,
    correlation: RouteCorrelation<Install, Verify>,
    deadline: HopDeadline,
) -> TerminalWorkerRequest<WTerminalInputRouteResult>
where
    Install: FnOnce(&str) -> Result<(), String>,
    Verify: FnOnce() -> bool,
{
    send_route_control(
        RouteSend {
            workers,
            pending_rpcs,
            worker,
            worker_epoch: &message.worker_epoch,
            deadline,
            noun: "terminal input route",
            dropped: "worker transport dropped terminal input route claim",
        },
        correlation,
        |request_id, budget_ms| {
            CoordWorkerDownstream::TerminalInputRouteClaim(DTerminalInputRouteClaim {
                request_id: request_id.to_owned(),
                session_id: message.session_id.clone(),
                device_fingerprint: message.device_fingerprint.clone(),
                tab_id: message.tab_id.clone(),
                browser_connection_id: message.browser_connection_id.clone(),
                revision: message.revision,
                budget_ms,
                worker_epoch: message.worker_epoch.clone(),
                __buffa_unknown_fields: Default::default(),
            })
        },
    )
}

/// Allocate the coordinator's request id, then send one empty probe.
pub(crate) fn send_terminal_transport_probe_request<Install, Verify>(
    workers: &WorkerRegistry,
    pending_rpcs: &Arc<PendingRpcs>,
    worker: &Arc<WorkerHandle>,
    worker_epoch: &str,
    correlation: RouteCorrelation<Install, Verify>,
    deadline: HopDeadline,
) -> TerminalWorkerRequest<WTerminalTransportProbeResult>
where
    Install: FnOnce(&str) -> Result<(), String>,
    Verify: FnOnce() -> bool,
{
    send_route_control(
        RouteSend {
            workers,
            pending_rpcs,
            worker,
            worker_epoch,
            deadline,
            noun: "terminal transport probe",
            dropped: "worker transport dropped terminal transport probe",
        },
        correlation,
        |request_id, _budget_ms| {
            CoordWorkerDownstream::TerminalTransportProbe(DTerminalTransportProbe {
                request_id: request_id.to_owned(),
                worker_epoch: worker_epoch.to_owned(),
                __buffa_unknown_fields: Default::default(),
            })
        },
    )
}

/// Retire the worker's route actor for a closed Sync socket, on this exact
/// generation only. `false` when nothing reached the worker.
pub fn send_terminal_input_route_connection_closed(
    workers: &WorkerRegistry,
    worker: &Arc<WorkerHandle>,
    worker_epoch: &str,
    socket_id: &str,
) -> bool {
    if !is_current_terminal_input_route_worker(workers, worker, worker_epoch) {
        return false;
    }
    let frame = CoordWorkerDownstream::TerminalViewSocketClosed(DTerminalViewSocketClosed {
        socket_id: socket_id.to_owned(),
        __buffa_unknown_fields: Default::default(),
    });
    worker.send(frame) != 0
}

/// Everything a route-control send shares with its sibling.
struct RouteSend<'a> {
    workers: &'a WorkerRegistry,
    pending_rpcs: &'a Arc<PendingRpcs>,
    worker: &'a Arc<WorkerHandle>,
    worker_epoch: &'a str,
    deadline: HopDeadline,
    noun: &'static str,
    dropped: &'static str,
}

fn send_route_control<T, Install, Verify>(
    send: RouteSend<'_>,
    correlation: RouteCorrelation<Install, Verify>,
    frame: impl FnOnce(&str, u32) -> CoordWorkerDownstream,
) -> TerminalWorkerRequest<T>
where
    T: TypedResult,
    Install: FnOnce(&str) -> Result<(), String>,
    Verify: FnOnce() -> bool,
{
    let RouteSend {
        workers,
        pending_rpcs,
        worker,
        worker_epoch,
        deadline,
        noun,
        dropped,
    } = send;
    if !is_current_terminal_input_route_worker(workers, worker, worker_epoch) {
        return TerminalWorkerRequest::unsent(&format!("{noun} worker is unavailable"), false);
    }
    let Some(budget_ms) = worker_budget_ms(&deadline) else {
        return TerminalWorkerRequest::unsent(&format!("{noun} budget expired before send"), true);
    };
    let worker_fp = worker.worker_fp.as_str();
    let pending = match pending_rpcs.create_fresh(Some(worker_fp), crate::serve::now_ms()) {
        Ok(pending) => pending,
        Err(error) => return TerminalWorkerRequest::uncorrelated(error),
    };
    let request_id = pending.request_id().to_owned();
    let refusal = match (correlation.install)(&request_id) {
        Err(reason) => Some(reason),
        Ok(()) => (!(correlation.still_pending)()
            || !is_current_terminal_input_route_worker(workers, worker, worker_epoch))
        .then(|| format!("{noun} control was cancelled before send")),
    };
    if let Some(reason) = refusal {
        pending_rpcs.reject_unavailable(&request_id, &reason, Some(worker_fp));
        return TerminalWorkerRequest::from_pending(pending, &deadline, false);
    }
    let admitted = worker.send(frame(&request_id, budget_ms)) != 0;
    if !admitted {
        pending_rpcs.reject_unavailable(&request_id, dropped, Some(worker_fp));
    }
    TerminalWorkerRequest::from_pending(pending, &deadline, admitted)
}
