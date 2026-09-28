//! Typed direct-control results a worker returns, handed to the owner waiting
//! for them together with the exact worker generation that sent them, so a
//! late frame from a replaced connection cannot settle its successor's control.
//! Called by `worker_link::live_frames::handle_rpc` after its fence and
//! readiness checks. Ports the terminal-route half of `apps/coord/src/terminal/
//! direct/worker-frame-dispatch-direct-results.ts` and `-direct-terminal.ts`.

use std::sync::Arc;

use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::services::CoordServices;
use crate::worker_link::dispatch::DispatchOutcome;

/// Settle one typed direct result against its owner. A result that matches no
/// pending control -- stale, from another generation, or not answering what
/// was asked -- is consumed and changes nothing.
pub(crate) fn accept_direct_result(
    services: &CoordServices,
    source: &Arc<WorkerHandle>,
    upstream: CoordWorkerUpstream,
) -> DispatchOutcome {
    let route_results = services.terminal_input.route_results();
    let settled = match &upstream {
        CoordWorkerUpstream::TerminalInputRouteResult(result) => {
            route_results.accept_input_route_result(source, result)
        }
        CoordWorkerUpstream::TerminalTransportProbeResult(result) => {
            route_results.accept_transport_probe_result(source, result)
        }
        _ => return DispatchOutcome::Refused,
    };
    if !settled {
        tracing::debug!(worker_fp = %source.worker_fp, what = upstream.kind(),
            "worker link: a typed direct result matched no pending control; dropped");
    }
    DispatchOutcome::Handled
}
