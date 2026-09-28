//! Process-wide serialization of global-search batches per worker: one page
//! scans a worker at a time, distinct workers run in parallel.
//!
//! Ports `apps/coord/src/search/global-search-worker-lanes.ts`
//! (`GlobalSearchWorkerLaneOwner`). One owner lives on `services.search`, so
//! separate browsers cannot overlap a worker; `search::rpc_search` acquires a
//! lease per worker group. Queue admission is bounded, every wait spends the
//! caller's page deadline, and dropping a lease is its release.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Waiters one worker's lane queues before a further page is refused outright.
pub const GLOBAL_SEARCH_MAX_WAITERS_PER_WORKER: usize = 32;

#[derive(Debug)]
struct LaneWaiter {
    id: u64,
    deadline: Instant,
    abort: CancellationToken,
    grant: oneshot::Sender<GlobalSearchWorkerLease>,
}

#[derive(Debug, Default)]
struct LaneState {
    active_token: Option<u64>,
    waiters: VecDeque<LaneWaiter>,
}

#[derive(Debug, Default)]
struct LaneTable {
    lanes: Mutex<HashMap<String, LaneState>>,
    sequence: AtomicU64,
}

impl LaneTable {
    fn lanes(&self) -> MutexGuard<'_, HashMap<String, LaneState>> {
        self.lanes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn next_id(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// The right to scan one worker until dropped.
#[derive(Debug)]
pub struct GlobalSearchWorkerLease {
    table: Arc<LaneTable>,
    worker_fp: String,
    token: u64,
    released: bool,
}

impl Drop for GlobalSearchWorkerLease {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        release(&self.table, &self.worker_fp, self.token);
    }
}

/// Serializes global-search pages per worker.
#[derive(Debug, Clone, Default)]
pub struct GlobalSearchWorkerLaneOwner {
    table: Arc<LaneTable>,
}

impl GlobalSearchWorkerLaneOwner {
    /// No lane held.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The monotonic instant `duration_ms` from now.
    #[must_use]
    pub fn deadline_after(&self, duration_ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(duration_ms)
    }

    /// What is left of a deadline; zero once it passed.
    #[must_use]
    pub fn remaining(&self, deadline: Instant) -> Duration {
        deadline.saturating_duration_since(Instant::now())
    }

    /// Queue for a worker's lane. Admission is decided NOW, when this is
    /// called, not when the returned future is first polled: a full queue is
    /// refused before anything waits. `None` for a spent deadline, a fired
    /// `abort`, a full queue, or a wait that outlived its deadline.
    pub fn acquire(
        &self,
        worker_fp: &str,
        deadline: Instant,
        abort: &CancellationToken,
    ) -> impl Future<Output = Option<GlobalSearchWorkerLease>> + Send + 'static {
        let admission = self.admit(worker_fp, deadline, abort);
        let table = Arc::clone(&self.table);
        let worker_fp = worker_fp.to_owned();
        let abort = abort.clone();
        async move {
            match admission {
                Admission::Refused => None,
                Admission::Granted(lease) => Some(lease),
                Admission::Queued {
                    waiter_id,
                    receiver,
                } => {
                    let mut queued = QueuedWaiter {
                        table,
                        worker_fp,
                        waiter_id,
                        receiver,
                    };
                    queued.wait(deadline, &abort).await
                }
            }
        }
    }

    fn admit(&self, worker_fp: &str, deadline: Instant, abort: &CancellationToken) -> Admission {
        if self.remaining(deadline).is_zero() || abort.is_cancelled() {
            return Admission::Refused;
        }
        let mut lanes = self.table.lanes();
        let lane = lanes.entry(worker_fp.to_owned()).or_default();
        if lane.active_token.is_none() {
            let token = self.table.next_id();
            lane.active_token = Some(token);
            return Admission::Granted(GlobalSearchWorkerLease {
                table: Arc::clone(&self.table),
                worker_fp: worker_fp.to_owned(),
                token,
                released: false,
            });
        }
        if lane.waiters.len() >= GLOBAL_SEARCH_MAX_WAITERS_PER_WORKER {
            tracing::debug!(worker_fp, "global_search_lane_queue_full");
            return Admission::Refused;
        }
        let (grant, receiver) = oneshot::channel();
        let waiter_id = self.table.next_id();
        lane.waiters.push_back(LaneWaiter {
            id: waiter_id,
            deadline,
            abort: abort.clone(),
            grant,
        });
        Admission::Queued {
            waiter_id,
            receiver,
        }
    }
}

enum Admission {
    Refused,
    Granted(GlobalSearchWorkerLease),
    Queued {
        waiter_id: u64,
        receiver: oneshot::Receiver<GlobalSearchWorkerLease>,
    },
}

/// A queued wait; dropping it before a grant leaves the queue.
struct QueuedWaiter {
    table: Arc<LaneTable>,
    worker_fp: String,
    waiter_id: u64,
    receiver: oneshot::Receiver<GlobalSearchWorkerLease>,
}

impl QueuedWaiter {
    async fn wait(
        &mut self,
        deadline: Instant,
        abort: &CancellationToken,
    ) -> Option<GlobalSearchWorkerLease> {
        tokio::select! {
            granted = &mut self.receiver => granted.ok(),
            () = tokio::time::sleep_until(deadline) => self.retire(),
            () = abort.cancelled() => self.retire(),
        }
    }

    /// Leave the queue; a grant that raced the timer or the abort is kept,
    /// because a grant is only ever made under the same lock.
    fn retire(&mut self) -> Option<GlobalSearchWorkerLease> {
        let mut lanes = self.table.lanes();
        if let Some(lane) = lanes.get_mut(&self.worker_fp)
            && let Some(index) = lane.waiters.iter().position(|w| w.id == self.waiter_id)
        {
            lane.waiters.remove(index);
            return None;
        }
        drop(lanes);
        self.receiver.try_recv().ok()
    }
}

impl Drop for QueuedWaiter {
    fn drop(&mut self) {
        let mut lanes = self.table.lanes();
        if let Some(lane) = lanes.get_mut(&self.worker_fp) {
            lane.waiters.retain(|waiter| waiter.id != self.waiter_id);
        }
    }
}

/// Hand the lane to the next live waiter, or retire it.
fn release(table: &Arc<LaneTable>, worker_fp: &str, token: u64) {
    let mut lanes = table.lanes();
    let Some(lane) = lanes.get_mut(worker_fp) else {
        return;
    };
    if lane.active_token != Some(token) {
        return;
    }
    while let Some(waiter) = lane.waiters.pop_front() {
        if waiter.abort.is_cancelled() || Instant::now() >= waiter.deadline {
            continue;
        }
        let next = table.next_id();
        lane.active_token = Some(next);
        let lease = GlobalSearchWorkerLease {
            table: Arc::clone(table),
            worker_fp: worker_fp.to_owned(),
            token: next,
            released: false,
        };
        match waiter.grant.send(lease) {
            Ok(()) => return,
            // The waiter is gone; this lease was never held, so it must not
            // release (the lock is held here).
            Err(mut unheld) => unheld.released = true,
        }
    }
    lanes.remove(worker_fp);
}
