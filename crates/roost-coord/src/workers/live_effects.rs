//! The workers half of `LiveEffects`: the browser-command reap a committed
//! snapshot owes the workers it dropped.
//!
//! Ported from `dispatchSnapshotOrphanReaps` in
//! `apps/coord/src/events/event-transaction.ts:323-348`.
//!
//! WHY THE INDEX HALF IS DELEGATED AND NOT REIMPLEMENTED. A committed event's
//! durable channel index belongs to the terminal byte hub -- it owns the key and
//! route-cache maps a `respawned` rebinds and a `closed` prunes, and
//! `coord_core::seams::WorkerRouteIndex` deliberately exposes only the three
//! coarse operations this domain needs. This value therefore composes: the reap
//! is the workers domain's because it is a browser command, and the index is
//! whatever the terminal domain installed. Neither half has a default body, so a
//! coordinator cannot be wired with one and silently miss the other.

use std::sync::Arc;

use roost_protocol::wire::{SessionEvent, SessionId, WorkerFp};

use crate::coord_core::worker_handle::WorkerRegistry;
use crate::events::append::LiveEffects;

use super::send::reap_orphan_pty;

/// The reap, over the workers domain's send path.
#[derive(Clone)]
pub struct WorkerLiveEffects {
    /// Where a coordinator-originated command goes.
    workers: Arc<WorkerRegistry>,
    /// The terminal domain's durable channel index.
    channels: Arc<dyn LiveEffects>,
}

impl WorkerLiveEffects {
    /// Compose the workers reap over the terminal channel index.
    #[must_use]
    pub fn new(workers: Arc<WorkerRegistry>, channels: Arc<dyn LiveEffects>) -> Self {
        Self { workers, channels }
    }
}

impl std::fmt::Debug for WorkerLiveEffects {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerLiveEffects")
            .field("workers", &self.workers)
            .finish_non_exhaustive()
    }
}

impl LiveEffects for WorkerLiveEffects {
    fn index_durable_channel(
        &self,
        event: &SessionEvent,
        authenticated_worker_fp: Option<&WorkerFp>,
    ) {
        self.channels
            .index_durable_channel(event, authenticated_worker_fp);
    }

    fn kill_orphan_pty(&self, worker_fp: &WorkerFp, session_id: &str) {
        // A session id that is not a uuid cannot be addressed on a worker, and
        // a kill that names a different PTY is worse than no kill at all, so
        // this is reported rather than coerced.
        let Ok(session_id) = SessionId::try_from(session_id) else {
            tracing::warn!(
                %worker_fp, session_id, "a force-closed session id is not addressable"
            );
            return;
        };
        reap_orphan_pty(&self.workers, worker_fp, &session_id);
    }
}
