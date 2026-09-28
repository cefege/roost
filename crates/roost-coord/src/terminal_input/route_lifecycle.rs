//! How the worker link's transitions reach the input-route owner: a hello is
//! acknowledged `terminal-input-route-v1` only because this owner exists, a
//! superseded, revoked or closed generation's controls are cancelled, and a
//! generation that became ready receives the retirements retained for it.
//! Registered on `CoordServices::worker_lifecycle`. Ports the route-owner calls
//! of `apps/coord/src/workers/worker-conn.ts` and `main.ts` `onWorkerConnected`.

use std::collections::BTreeSet;
use std::sync::Arc;

use roost_protocol::versioning::CAPABILITY_TERMINAL_INPUT_ROUTE_V1;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::coord_core::worker_lifecycle::{LinkEnd, WorkerLifecycleObserver};
use crate::terminal_input::route_results::TerminalInputRouteResults;

impl WorkerLifecycleObserver for TerminalInputRouteResults {
    fn acknowledge_capabilities(&self, advertised: &BTreeSet<String>) -> Vec<&'static str> {
        if advertised.contains(CAPABILITY_TERMINAL_INPUT_ROUTE_V1) {
            vec![CAPABILITY_TERMINAL_INPUT_ROUTE_V1]
        } else {
            Vec::new()
        }
    }

    fn on_ready(&self, handle: &Arc<WorkerHandle>) {
        self.flush_worker_retirements(&handle.worker_fp);
    }

    fn on_superseded(&self, superseded: &Arc<WorkerHandle>) {
        self.cancel_for_worker_handle(superseded, "connection_superseded");
    }

    fn on_closed(&self, handle: &Arc<WorkerHandle>, end: LinkEnd) {
        let reason = match end {
            LinkEnd::Revoked => "worker_revoked",
            LinkEnd::Closed { .. } => "worker_disconnected",
        };
        self.cancel_for_worker_handle(handle, reason);
    }
}
