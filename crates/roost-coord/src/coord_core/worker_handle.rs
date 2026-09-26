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
// reads `ready`/`revoked` and sends through them.
//
// THE ARC IS THE FENCE. Retiring a generation swaps the registry's entry and
// drops its handle, so a holder of a stale handle can no longer reach the
// socket. That is why `send` is a private closure field rather than something
// the caller passes back: a caller cannot forge or outlive the transport it was
// given.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use roost_protocol::wire::WorkerFp;

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
    /// Whether the generation completed its handshake.
    pub ready: bool,
    /// Whether a later generation fenced this one.
    pub revoked: bool,
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
            .field("ready", &self.ready)
            .field("revoked", &self.revoked)
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
            ready: false,
            revoked: false,
            send,
        }
    }

    /// Write one downstream frame, returning its delivery sequence.
    pub fn send(&self, frame: CoordWorkerDownstream) -> i64 {
        (self.send)(frame)
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

    /// Register a generation, replacing whatever the worker had before.
    pub fn insert(&self, handle: Arc<WorkerHandle>) {
        if let Ok(mut handles) = self.handles.write() {
            handles.insert(handle.worker_fp.clone(), handle);
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
        (handle.ready && !handle.revoked).then_some(handle)
    }

    /// Drop a worker's socket, so every outstanding handle for it stops
    /// reaching anything.
    pub fn retire(&self, worker_fp: &WorkerFp) {
        if let Ok(mut handles) = self.handles.write() {
            handles.remove(worker_fp);
        }
    }

    /// Mark a generation fenced without dropping it, so a send already in
    /// flight is refused rather than silently succeeding.
    pub fn fence(&self, worker_fp: &WorkerFp) {
        if let Ok(handles) = self.handles.read() {
            if let Some(handle) = handles.get(worker_fp) {
                tracing::warn!(
                    worker_fp = %worker_fp,
                    connection_generation = %handle.connection_generation,
                    "a worker generation is fenced"
                );
            }
        }
    }
}
