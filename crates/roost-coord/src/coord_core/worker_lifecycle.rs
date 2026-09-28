//! The worker link's lifecycle, as every other domain needs to hear it: which
//! capabilities a hello may be acknowledged with, and the four transitions of
//! one socket generation (acknowledged, ready, superseded, closed).
//!
//! Called by `worker_link::link_session` at each transition; observers are
//! registered on `CoordServices::worker_lifecycle`. Ports the per-owner calls
//! `apps/coord/src/workers/worker-conn.ts` makes inline at hello, supersede,
//! revoke and close, so a later owner registers here instead of editing the link.

use std::collections::BTreeSet;
use std::sync::{Arc, PoisonError, RwLock};

use crate::coord_core::worker_handle::WorkerHandle;
use crate::terminal_screen::pending_rpcs::PendingRpcs;

/// How one socket generation ended, which decides what an owner releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkEnd {
    /// The worker's credential was fenced under the open socket (v2 `revoke`).
    Revoked,
    /// The socket closed. `replaced` is whether a newer generation already
    /// claims the fingerprint, in which case it owns the worker's in-flight work
    /// (v2 `close`, `worker-conn.ts:198-205`).
    Closed { replaced: bool },
}

/// One owner of per-worker state the link's transitions must reach.
///
/// Every method has an empty default because an owner implements only the
/// transitions it holds state for; the registry, not the observer, is what
/// guarantees each transition is announced.
pub trait WorkerLifecycleObserver: Send + Sync {
    /// The capabilities this owner acknowledges out of a hello's advertised set.
    /// v2 acknowledges a capability only when the owner that serves it exists
    /// (`worker-conn.ts:310-321`), so an owner that is not registered is a
    /// capability the worker is never told it may use.
    fn acknowledge_capabilities(&self, _advertised: &BTreeSet<String>) -> Vec<&'static str> {
        Vec::new()
    }

    /// A hello was admitted and its generation claimed the fingerprint.
    fn on_hello_acknowledged(&self, _handle: &Arc<WorkerHandle>) {}

    /// The generation's exact snapshot committed; it is routable.
    fn on_ready(&self, _handle: &Arc<WorkerHandle>) {}

    /// A newer hello replaced this generation.
    fn on_superseded(&self, _superseded: &Arc<WorkerHandle>) {}

    /// The generation ended.
    fn on_closed(&self, _handle: &Arc<WorkerHandle>, _end: LinkEnd) {}
}

/// Every registered lifecycle observer, called in registration order.
pub struct WorkerLifecycle {
    observers: RwLock<Vec<Arc<dyn WorkerLifecycleObserver>>>,
}

impl std::fmt::Debug for WorkerLifecycle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerLifecycle")
            .field("observers", &self.snapshot().len())
            .finish()
    }
}

impl WorkerLifecycle {
    /// A registry over the observers the process is built with.
    #[must_use]
    pub fn new(observers: Vec<Arc<dyn WorkerLifecycleObserver>>) -> Self {
        Self {
            observers: RwLock::new(observers),
        }
    }

    /// Add an owner whose runtime is built after the services are.
    pub fn register(&self, observer: Arc<dyn WorkerLifecycleObserver>) {
        self.observers
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .push(observer);
    }

    /// The capabilities the registered owners acknowledge, in registration order.
    #[must_use]
    pub fn acknowledged_capabilities(&self, advertised: &BTreeSet<String>) -> Vec<&'static str> {
        self.snapshot()
            .iter()
            .flat_map(|observer| observer.acknowledge_capabilities(advertised))
            .collect()
    }

    /// Announce an admitted hello.
    pub fn hello_acknowledged(&self, handle: &Arc<WorkerHandle>) {
        self.snapshot()
            .iter()
            .for_each(|observer| observer.on_hello_acknowledged(handle));
    }

    /// Announce a generation crossing its snapshot barrier.
    pub fn ready(&self, handle: &Arc<WorkerHandle>) {
        self.snapshot()
            .iter()
            .for_each(|observer| observer.on_ready(handle));
    }

    /// Announce a generation a newer hello replaced.
    pub fn superseded(&self, superseded: &Arc<WorkerHandle>) {
        self.snapshot()
            .iter()
            .for_each(|observer| observer.on_superseded(superseded));
    }

    /// Announce a generation's end.
    pub fn closed(&self, handle: &Arc<WorkerHandle>, end: LinkEnd) {
        self.snapshot()
            .iter()
            .for_each(|observer| observer.on_closed(handle, end));
    }

    /// The observers, copied out so no callback runs under the registry lock.
    fn snapshot(&self) -> Vec<Arc<dyn WorkerLifecycleObserver>> {
        self.observers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// In-flight coordinator→worker requests fail fast when the generation that
/// would have answered them is gone, instead of hanging to their deadline
/// (v2 `rejectPendingRpcsForWorker`, `worker-conn.ts:183,202,272`).
impl WorkerLifecycleObserver for PendingRpcs {
    fn on_superseded(&self, superseded: &Arc<WorkerHandle>) {
        self.reject_pending_for(superseded, "worker connection superseded");
    }

    fn on_closed(&self, handle: &Arc<WorkerHandle>, end: LinkEnd) {
        match end {
            LinkEnd::Revoked => self.reject_pending_for(handle, "worker credential revoked"),
            LinkEnd::Closed { replaced: false } => {
                self.reject_pending_for(handle, "worker disconnected");
            }
            // The replacement owns the fingerprint's in-flight requests.
            LinkEnd::Closed { replaced: true } => {}
        }
    }
}

impl PendingRpcs {
    fn reject_pending_for(&self, handle: &WorkerHandle, message: &str) {
        let rejected = self.reject_all_for_worker(handle.worker_fp.as_str(), message);
        if rejected > 0 {
            tracing::info!(
                worker_fp = %handle.worker_fp,
                rejected,
                message,
                "in-flight worker requests were rejected"
            );
        }
    }
}
