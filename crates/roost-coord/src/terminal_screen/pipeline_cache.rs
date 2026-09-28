//! Handler-owned, target-scoped terminal-pipeline sampling: each worker is
//! sampled at most once per 2 s window, concurrent collections batch only their
//! uncached targets, a cached record never answers another target's scope, and
//! a replaced worker generation starts fresh. Ports
//! `apps/coord/src/terminal/screen/worker-terminal-pipeline-cache.ts`. Owned by
//! the DiagSnapshot handler; requests go through `pipeline_request`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::future::join_all;
use roost_protocol::wire::WorkerFp;
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep_until};

use crate::terminal_screen::pipeline_cache_entry::{
    CacheEntries, PendingTargetCollection, WorkerPipelineCacheEntry, offline,
};
use crate::terminal_screen::pipeline_request::{
    WorkerTerminalPipelineSnapshotResult, collect_worker_terminal_pipeline_snapshots,
};
use crate::terminal_screen::pipeline_snapshot::{
    TerminalPipelineDiagnosticTarget, normalize_terminal_pipeline_diagnostic_targets,
};
use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::workers::send::current_routable_worker;

/// How long one worker sample answers its targets, and the least time between
/// two samples of one worker.
pub const WORKER_TERMINAL_PIPELINE_CACHE_MS: u64 = 2_000;

type Target = TerminalPipelineDiagnosticTarget;
type SampleResult = WorkerTerminalPipelineSnapshotResult;

/// One handler's sampling state. Time is `tokio::time::Instant`, so a test
/// pauses and advances the cache window instead of sleeping through it.
pub struct WorkerTerminalPipelineSnapshotCache {
    state: Arc<CacheState>,
}

struct CacheState {
    relay: ScrollbackRelay,
    entries: Mutex<CacheEntries>,
}

impl std::fmt::Debug for WorkerTerminalPipelineSnapshotCache {
    /// Entries hold oneshot senders and socket handles; a log line needs how
    /// many workers are cached.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerTerminalPipelineSnapshotCache")
            .field("workers", &self.state.lock().by_worker.len())
            .finish_non_exhaustive()
    }
}

impl WorkerTerminalPipelineSnapshotCache {
    /// An empty cache over the relay's registry, pending table and clock.
    #[must_use]
    pub fn new(relay: ScrollbackRelay) -> Self {
        let entries = Mutex::new(CacheEntries::default());
        Self {
            state: Arc::new(CacheState { relay, entries }),
        }
    }

    /// One result per worker whose targets normalize to something; cached
    /// targets answer at once, the rest wait for their worker's next batch.
    pub async fn collect(
        &self,
        targets_by_worker: &BTreeMap<WorkerFp, Vec<Target>>,
        timeout_ms: Option<u64>,
    ) -> BTreeMap<WorkerFp, SampleResult> {
        let requests: Vec<(&WorkerFp, Vec<Target>)> = targets_by_worker
            .iter()
            .map(|(worker_fp, targets)| {
                (
                    worker_fp,
                    normalize_terminal_pipeline_diagnostic_targets(targets),
                )
            })
            .filter(|(_, targets)| !targets.is_empty())
            .collect();
        let results = join_all(requests.iter().map(|(worker_fp, targets)| {
            self.state
                .collect_worker(worker_fp, targets.clone(), timeout_ms)
        }))
        .await;
        requests
            .into_iter()
            .map(|(worker_fp, _)| worker_fp.clone())
            .zip(results)
            .collect()
    }

    /// Settles every waiting collection as offline and forgets every sample.
    pub fn dispose(&self) {
        let mut entries = self.state.lock();
        let worker_fps: Vec<WorkerFp> = entries.by_worker.keys().cloned().collect();
        for worker_fp in worker_fps {
            entries.clear_entry(&worker_fp, None, "pipeline cache disposed");
        }
    }
}

impl Drop for WorkerTerminalPipelineSnapshotCache {
    /// Spawned timers hold the state, so dropping the cache disposes it and
    /// they wake to nothing.
    fn drop(&mut self) {
        self.dispose();
    }
}

impl CacheState {
    fn lock(&self) -> MutexGuard<'_, CacheEntries> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn collect_worker(
        self: &Arc<Self>,
        worker_fp: &WorkerFp,
        targets: Vec<Target>,
        timeout_ms: Option<u64>,
    ) -> SampleResult {
        let Some(worker) = current_routable_worker(self.relay.workers(), worker_fp) else {
            return offline("worker is not connected");
        };
        let receiver = {
            let mut entries = self.lock();
            let replaced = entries
                .by_worker
                .get(worker_fp)
                .is_some_and(|entry| !Arc::ptr_eq(&entry.worker, &worker));
            if replaced {
                entries.clear_entry(worker_fp, None, "worker connection changed");
            }
            if !entries.by_worker.contains_key(worker_fp) {
                entries.next_entry_id += 1;
                let entry = WorkerPipelineCacheEntry::new(entries.next_entry_id, worker);
                entries.by_worker.insert(worker_fp.clone(), entry);
                tracing::debug!(worker_fp = %worker_fp, "terminal pipeline cache entry opened");
            }
            let Some(entry) = entries.by_worker.get_mut(worker_fp) else {
                return offline("worker is not connected");
            };
            if let Some(cached) = entry.project_cached_targets(&targets, Instant::now()) {
                return cached;
            }
            let (reply, receiver) = oneshot::channel();
            entry
                .pending
                .push(PendingTargetCollection { targets, reply });
            let entry_id = entry.entry_id;
            self.schedule_batch(&mut entries, worker_fp, entry_id, timeout_ms);
            receiver
        };
        receiver
            .await
            .unwrap_or_else(|_| offline("terminal pipeline collection failed"))
    }

    /// Arms the next batch for this entry: after the cache window when the
    /// worker was sampled recently, otherwise as soon as the caller yields, so
    /// collections that arrive together share one request.
    fn schedule_batch(
        self: &Arc<Self>,
        entries: &mut CacheEntries,
        worker_fp: &WorkerFp,
        entry_id: u64,
        timeout_ms: Option<u64>,
    ) {
        let Some(entry) = entries.current(worker_fp, entry_id) else {
            return;
        };
        if entry.in_flight || entry.pending.is_empty() {
            return;
        }
        let state = Arc::clone(self);
        let worker_fp = worker_fp.clone();
        if let Some(next_request_at) = entry.next_request_at.filter(|at| *at > Instant::now()) {
            if entry.request_timer_armed {
                return;
            }
            entry.request_timer_armed = true;
            tokio::spawn(async move {
                sleep_until(next_request_at).await;
                let mut entries = state.lock();
                if let Some(entry) = entries.current(&worker_fp, entry_id) {
                    entry.request_timer_armed = false;
                }
                state.schedule_batch(&mut entries, &worker_fp, entry_id, timeout_ms);
            });
            return;
        }
        if entry.flush_scheduled {
            return;
        }
        entry.flush_scheduled = true;
        tokio::spawn(async move { state.flush_batch(worker_fp, entry_id, timeout_ms).await });
    }

    async fn flush_batch(
        self: Arc<Self>,
        worker_fp: WorkerFp,
        entry_id: u64,
        timeout_ms: Option<u64>,
    ) {
        let batch = {
            let mut entries = self.lock();
            let Some(entry) = entries.current(&worker_fp, entry_id) else {
                return;
            };
            entry.flush_scheduled = false;
            if entry.in_flight {
                return;
            }
            let batch = entry.uncached_batch(Instant::now());
            entry.resolve_satisfied_pending(Instant::now());
            if batch.is_empty() {
                self.schedule_batch(&mut entries, &worker_fp, entry_id, timeout_ms);
                return;
            }
            entry.in_flight = true;
            batch
        };
        tracing::debug!(worker_fp = %worker_fp, targets = batch.len(), "terminal pipeline cache batch sent");
        let request = BTreeMap::from([(worker_fp.clone(), batch.clone())]);
        let mut results =
            collect_worker_terminal_pipeline_snapshots(&self.relay, &request, timeout_ms).await;
        let result = results
            .remove(&worker_fp)
            .unwrap_or_else(|| offline("worker pipeline request was not collected"));
        let mut entries = self.lock();
        let Some(entry) = entries.current(&worker_fp, entry_id) else {
            return;
        };
        entry.in_flight = false;
        let now = Instant::now();
        let expires_at = now + Duration::from_millis(WORKER_TERMINAL_PIPELINE_CACHE_MS);
        entry.next_request_at = Some(expires_at);
        let answered = matches!(result, SampleResult::Ok { .. });
        tracing::debug!(worker_fp = %worker_fp, answered, "terminal pipeline cache batch settled");
        match result {
            SampleResult::Ok {
                response_ms,
                snapshot,
            } => {
                entry.store_samples(&batch, response_ms, snapshot, expires_at);
                entry.resolve_satisfied_pending(now);
            }
            error @ SampleResult::Error { .. } => {
                entry.settle_pending(&error);
                entry.cached_error = Some((error, expires_at));
            }
        }
        self.arm_expiry(&mut entries, &worker_fp, entry_id);
        if answered {
            self.schedule_batch(&mut entries, &worker_fp, entry_id, timeout_ms);
        }
    }

    /// Forgets expired samples at their deadline, and the whole entry once
    /// nothing is cached, waiting or in flight.
    fn arm_expiry(
        self: &Arc<Self>,
        entries: &mut CacheEntries,
        worker_fp: &WorkerFp,
        entry_id: u64,
    ) {
        let Some(entry) = entries.current(worker_fp, entry_id) else {
            return;
        };
        let now = Instant::now();
        let error_deadline = entry
            .cached_error
            .as_ref()
            .map(|(_, expires_at)| *expires_at);
        let sample_deadlines = entry.targets.values().map(|sample| sample.expires_at);
        let Some(deadline) = error_deadline
            .into_iter()
            .chain(sample_deadlines)
            .filter(|at| *at > now)
            .min()
        else {
            if entry.is_idle() {
                entries.clear_entry(worker_fp, Some(entry_id), "pipeline cache expired");
            }
            return;
        };
        entry.expiry_token += 1;
        let expiry_token = entry.expiry_token;
        let state = Arc::clone(self);
        let worker_fp = worker_fp.clone();
        tokio::spawn(async move {
            sleep_until(deadline).await;
            let mut entries = state.lock();
            let Some(entry) = entries.current(&worker_fp, entry_id) else {
                return;
            };
            if entry.expiry_token != expiry_token {
                return;
            }
            let now = Instant::now();
            entry.targets.retain(|_, sample| sample.expires_at > now);
            if entry
                .cached_error
                .as_ref()
                .is_some_and(|(_, expires_at)| *expires_at <= now)
            {
                entry.cached_error = None;
            }
            if entry.is_idle() && entry.targets.is_empty() && entry.cached_error.is_none() {
                entries.clear_entry(&worker_fp, Some(entry_id), "pipeline cache expired");
                return;
            }
            state.arm_expiry(&mut entries, &worker_fp, entry_id);
        });
    }
}
