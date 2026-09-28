//! The terminal input domain: Sync and unary input batches through bounded
//! sender lanes to exactly one keeper write each, their durable audit, and the
//! typed input-route claims and probes a direct terminal route depends on.
//! `TerminalInputRuntime` is built once on `CoordServices::terminal_input`.
//! Ports `apps/coord/src/terminal/input/session-control.ts`, the facade whose
//! exports this runtime and its modules are.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::coord_core::worker_handle::WorkerRegistry;
use crate::terminal_screen::pending_rpcs::PendingRpcs;

pub mod control_lane;
pub mod input_audit;
pub mod input_control;
pub mod route_contract;
pub mod route_lifecycle;
pub mod route_results;
pub mod route_retirements;
pub mod route_sender;
mod route_slots;
pub mod route_state;
pub mod rpc_input;
pub mod sync_controls;
mod sync_route_controls;
pub mod write_control;

use control_lane::ControlLanes;
use input_audit::InputAuditQueue;
use route_results::TerminalInputRouteResults;

/// The process's terminal input state.
#[derive(Debug)]
pub struct TerminalInputRuntime {
    lanes: Arc<ControlLanes>,
    audit: InputAuditQueue,
    route_results: Arc<TerminalInputRouteResults>,
    compatibility_input_seq: AtomicU64,
}

impl TerminalInputRuntime {
    /// A runtime over the process's worker registry and the pending-request
    /// table its typed route results settle.
    #[must_use]
    pub fn new(workers: Arc<WorkerRegistry>, pending_rpcs: Arc<PendingRpcs>) -> Self {
        Self {
            lanes: Arc::new(ControlLanes::new()),
            audit: InputAuditQueue::new(),
            route_results: Arc::new(TerminalInputRouteResults::new(workers, pending_rpcs)),
            compatibility_input_seq: AtomicU64::new(0),
        }
    }

    /// Every sender/session input lane.
    #[must_use]
    pub fn lanes(&self) -> &Arc<ControlLanes> {
        &self.lanes
    }

    /// The durable audit queue for Sync input outcomes.
    #[must_use]
    pub fn audit(&self) -> &InputAuditQueue {
        &self.audit
    }

    /// The typed route-control owner; also a worker lifecycle observer.
    #[must_use]
    pub fn route_results(&self) -> &Arc<TerminalInputRouteResults> {
        &self.route_results
    }

    /// The next sequence for a unary batch, which carries none of its own.
    pub fn next_compatibility_input_seq(&self) -> u64 {
        self.compatibility_input_seq.fetch_add(1, Ordering::Relaxed) + 1
    }
}
