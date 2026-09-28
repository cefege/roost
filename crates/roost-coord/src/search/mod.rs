//! Global session search across every worker, and the cancellation that
//! retires one.
//!
//! One field on `CoordServices`, reached as `core.services.search`: the cursor
//! owner (`cursors`) and the per-worker lanes (`worker_lanes`) v2 builds once
//! per router and injects into `handlers-sessions-global-search.ts`. The
//! correlation table and worker registry it sends through are the scrollback
//! relay's (`services.scrollback`), never a second copy.
//!
//! `new()` takes nothing and must keep taking nothing: anything the domain
//! needs from configuration is read at call time from `core.services.boot`.

pub mod cancel;
pub mod cursor_types;
pub mod cursors;
pub mod fanout;
pub mod options;
pub mod page_group;
pub mod rpc_search;
pub mod worker_lanes;
pub mod worker_result;

use crate::search::cursors::GlobalSearchCursorOwner;
use crate::search::worker_lanes::GlobalSearchWorkerLaneOwner;

/// The global-search state one coordinator process holds.
#[derive(Debug, Default)]
pub struct GlobalSearchRuntime {
    cursors: GlobalSearchCursorOwner,
    lanes: GlobalSearchWorkerLaneOwner,
}

impl GlobalSearchRuntime {
    /// A coordinator with no search in flight.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Continuation cursors, active selections, and cancel tombstones.
    #[must_use]
    pub fn cursors(&self) -> &GlobalSearchCursorOwner {
        &self.cursors
    }

    /// The per-worker serialization lanes.
    #[must_use]
    pub fn lanes(&self) -> &GlobalSearchWorkerLaneOwner {
        &self.lanes
    }
}
