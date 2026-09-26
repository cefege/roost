// The worker sockets themselves, as shared state rather than a collaborator.
//
// WHY THIS IS NOT A TRAIT. The terminal domain sends frames to workers through
// `connectWorkers`, and that is a `Map<WorkerFp, WorkerHandle>` of LIVE
// TRANSPORT OBJECTS: `send`, `close`, `fence`, `bufferedAmount`, plus mutable
// `revoked` and `ready` flags. Terminal code holds a handle ACROSS AN AWAIT and
// calls `send` on it afterwards. A `&dyn` cannot express that -- the handle is
// replaced on every hello and every reconnect, while a view hub or a stream
// dispatcher outlives hundreds of socket generations. Any `&mut dyn` handed
// over at construction is invalidated by the first reconnect.
//
// So this is shared vocabulary, not an inversion. Both sides name it: the
// workers domain registers, fences and retires handles; the terminal domain
// reads `is_routable` and sends through them.
//
// THE ARC IS THE FENCE, AND THE FENCE IS A FLAG BOTH SIDES CAN SEE. Retiring a
// generation drops the registry's entry AND sets the handle's `revoked`, because
// dropping the entry alone only stops a *lookup*: a holder that kept the
// `Arc<WorkerHandle>` across the retire would still reach the socket, and v2
// denies exactly that (`worker-registry.ts:30`, the `connectWorkers.get(workerFp)
// !== w` re-check inside every send). The flags are atomics rather than fields
// for the same reason: v2 mutates them from the transport owner while a
// different task reads them, and a plain `bool` behind an `Arc` cannot be.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

/// One authenticated worker socket generation.
#[derive(Clone)]
pub struct WorkerHandle {
    /// The worker this socket is.
    pub worker_fp: WorkerFp,
    /// The worker's process epoch, absent for a legacy self-hosted key.
    pub process_epoch: Option<String>,
    /// A fresh identity per connection, so a log line can tell a reconnect
    /// from a process restart.
    pub connection_generation: String,
    /// What this generation said it can do.
    pub capabilities: BTreeSet<String>,
    /// Whether the generation completed its snapshot barrier.
    ready: Arc<AtomicBool>,
    /// Whether a later generation, a consumed credential or a retirement fenced
    /// this one. Once set it is never cleared: a fence is permanent, so a stale
    /// handle cannot be revived by a reconnect.
    revoked: Arc<AtomicBool>,
    send: Arc<dyn Fn(CoordWorkerDownstream) -> i64 + Send + Sync>,
}

impl std::fmt::Debug for WorkerHandle {
    /// The transport is a closure and its debug output is not the registry's to
    /// choose; everything a log line needs about a generation is a plain field.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerHandle")
            .field("worker_fp", &self.worker_fp)
            .field("connection_generation", &self.connection_generation)
            .field("ready", &self.is_ready())
            .field("revoked", &self.is_revoked())
            .finish_non_exhaustive()
    }
}

impl WorkerHandle {
    /// A handle over a live socket.
    #[must_use]
    pub fn new(
        worker_fp: WorkerFp,
        process_epoch: Option<String>,
        connection_generation: String,
        capabilities: BTreeSet<String>,
        send: Arc<dyn Fn(CoordWorkerDownstream) -> i64 + Send + Sync>,
    ) -> Self {
        Self {
            worker_fp,
            process_epoch,
            connection_generation,
            capabilities,
            ready: Arc::new(AtomicBool::new(false)),
            revoked: Arc::new(AtomicBool::new(false)),
            send,
        }
    }

    /// Write one downstream frame, returning its delivery sequence.
    ///
    /// Zero for a fenced generation, which is the transport's own "dropped"
    /// answer and v2's answer too: `myHandle.send` returns 0 once `revoked` is
    /// set (`worker-conn.ts:64-72`), so a caller that kept this handle across a
    /// revocation cannot write to the socket. Readiness is NOT checked here --
    /// the transport owner sends its own handshake and keepalive before the
    /// snapshot barrier -- and only a caller addressing a frame by fingerprint
    /// goes through [`WorkerRegistry::current_routable`].
    pub fn send(&self, frame: CoordWorkerDownstream) -> i64 {
        if self.is_revoked() {
            return 0;
        }
        (self.send)(frame)
    }

    /// Whether this generation crossed its snapshot barrier.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Whether this generation has been fenced.
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }

    /// Whether a frame addressed by fingerprint may go to this generation: ready,
    /// and not fenced.
    #[must_use]
    pub fn is_routable(&self) -> bool {
        self.is_ready() && !self.is_revoked()
    }

    /// Cross the snapshot barrier, reporting whether this call was the one that
    /// did it. A generation that is already ready is not re-activated: a
    /// duplicate snapshot is not a reason to publish the worker again, and a
    /// fenced generation cannot be un-fenced by a late snapshot.
    pub fn mark_ready(&self) -> bool {
        !self.ready.swap(true, Ordering::AcqRel) && !self.is_revoked()
    }

    /// Fence this generation, so every outstanding handle for it stops reaching
    /// the socket.
    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }
}

/// Every worker's current socket, by fingerprint.
///
/// A test that wants no worker constructs an EMPTY registry. There is no no-op
/// value to write, which is strictly better than a trait impl that pretends a
/// worker exists and answers.
#[derive(Debug, Default)]
pub struct WorkerRegistry {
    handles: RwLock<HashMap<WorkerFp, Arc<WorkerHandle>>>,
}

impl WorkerRegistry {
    /// An empty registry: the state of a coordinator that has never seen a
    /// worker, which is also the state every test wants.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a generation, fencing whatever the worker had before.
    ///
    /// Fencing the replaced generation is not bookkeeping: a reconnect that
    /// arrives while the old socket is still open would otherwise leave two live
    /// generations for one fingerprint, and the old one can still write to a PTY
    /// the new one has already rebound.
    pub fn insert(&self, handle: Arc<WorkerHandle>) {
        if let Ok(mut handles) = self.handles.write()
            && let Some(previous) = handles.insert(handle.worker_fp.clone(), handle)
        {
            previous.revoke();
        }
    }

    /// The current generation, whether or not it is usable.
    #[must_use]
    pub fn current(&self, worker_fp: &WorkerFp) -> Option<Arc<WorkerHandle>> {
        self.handles
            .read()
            .ok()
            .and_then(|handles| handles.get(worker_fp).cloned())
    }

    /// The generation a frame should go to: ready, and not fenced.
    #[must_use]
    pub fn current_routable(&self, worker_fp: &WorkerFp) -> Option<Arc<WorkerHandle>> {
        let handle = self.current(worker_fp)?;
        handle.is_routable().then_some(handle)
    }

    /// Every fingerprint a frame can reach right now.
    ///
    /// Asked of [`Self::current_routable`] rather than filtered here, so the
    /// routable set and the send path cannot disagree about what "routable"
    /// means: this set is what a browser's online indicator reads, and a second
    /// copy of the predicate is how a fenced machine stays green.
    ///
    /// The whole set, in a stable order, because it is published whole on every
    /// connect and disconnect rather than folded per fingerprint
    /// (`worker-registry.ts:99-101`).
    #[must_use]
    pub fn routable_fps(&self) -> Vec<WorkerFp> {
        let Ok(handles) = self.handles.read() else {
            return Vec::new();
        };
        let mut routable: Vec<WorkerFp> = handles
            .keys()
            .filter(|worker_fp| self.current_routable(worker_fp).is_some())
            .cloned()
            .collect();
        routable.sort();
        routable
    }

    /// Fence a worker's current generation without dropping it, so a send
    /// already in flight is refused rather than silently succeeding.
    ///
    /// The entry stays, so a caller holding the handle can still see that it is
    /// the current one: that is what makes "fenced" and "disconnected"
    /// distinguishable in a log line and in the routable set, and it is the
    /// state v2's credential revocation leaves behind
    /// (`worker-conn.ts:169-186`, which sets `revoked` and never deletes).
    pub fn fence(&self, worker_fp: &WorkerFp) -> Option<Arc<WorkerHandle>> {
        let handle = self.current(worker_fp);
        if let Some(handle) = &handle {
            handle.revoke();
            tracing::warn!(
                worker_fp = %worker_fp,
                connection_generation = %handle.connection_generation,
                "a worker generation is fenced"
            );
        }
        handle
    }

    /// Drop a worker's socket, so every outstanding handle for it stops
    /// reaching anything.
    pub fn retire(&self, worker_fp: &WorkerFp) {
        if let Ok(mut handles) = self.handles.write()
            && let Some(previous) = handles.remove(worker_fp)
        {
            previous.revoke();
        }
    }
}
