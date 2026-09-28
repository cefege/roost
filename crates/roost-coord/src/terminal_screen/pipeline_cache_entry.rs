//! One worker generation's slice of the terminal-pipeline cache: its live
//! samples, cached error, waiting collections, and the projection that answers
//! a collection from cache alone. Split from `pipeline_cache` (which ports
//! `apps/coord/src/terminal/screen/worker-terminal-pipeline-cache.ts`) at the
//! entry boundary; only `pipeline_cache` reads it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use roost_proto::{TerminalPipelineSessionSnapshot, WTerminalPipelineSnapshot};
use roost_protocol::wire::WorkerFp;
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::terminal_screen::pipeline_request::{
    TerminalPipelineSnapshotErrorCode, WorkerTerminalPipelineSnapshotResult,
};
use crate::terminal_screen::pipeline_snapshot::{
    TERMINAL_PIPELINE_DIAG_MAX_TARGETS, TerminalPipelineDiagnosticTarget,
};

type Target = TerminalPipelineDiagnosticTarget;
type SampleResult = WorkerTerminalPipelineSnapshotResult;

#[derive(Default)]
pub(super) struct CacheEntries {
    pub(super) by_worker: HashMap<WorkerFp, WorkerPipelineCacheEntry>,
    pub(super) next_entry_id: u64,
}

pub(super) struct CachedTargetSample {
    pub(super) expires_at: Instant,
    pub(super) request_id: String,
    pub(super) response_ms: u64,
    pub(super) session: Option<TerminalPipelineSessionSnapshot>,
}

pub(super) struct PendingTargetCollection {
    pub(super) targets: Vec<Target>,
    pub(super) reply: oneshot::Sender<SampleResult>,
}

/// One worker generation's samples; `entry_id` is its identity, so a timer or
/// batch armed for a cleared entry cannot touch the entry that replaced it.
pub(super) struct WorkerPipelineCacheEntry {
    pub(super) entry_id: u64,
    pub(super) worker: Arc<WorkerHandle>,
    pub(super) cached_error: Option<(SampleResult, Instant)>,
    pub(super) expiry_token: u64,
    pub(super) flush_scheduled: bool,
    pub(super) in_flight: bool,
    pub(super) next_request_at: Option<Instant>,
    pub(super) pending: Vec<PendingTargetCollection>,
    pub(super) request_timer_armed: bool,
    pub(super) targets: HashMap<Target, CachedTargetSample>,
}

impl CacheEntries {
    pub(super) fn current(
        &mut self,
        worker_fp: &WorkerFp,
        entry_id: u64,
    ) -> Option<&mut WorkerPipelineCacheEntry> {
        self.by_worker
            .get_mut(worker_fp)
            .filter(|entry| entry.entry_id == entry_id)
    }

    /// Drops the entry (only `entry_id`'s, when named) and settles everything
    /// waiting on it as offline with `reason`.
    pub(super) fn clear_entry(
        &mut self,
        worker_fp: &WorkerFp,
        entry_id: Option<u64>,
        reason: &str,
    ) {
        let matches = self
            .by_worker
            .get(worker_fp)
            .is_some_and(|entry| entry_id.is_none_or(|entry_id| entry.entry_id == entry_id));
        if !matches {
            return;
        }
        if let Some(mut entry) = self.by_worker.remove(worker_fp) {
            entry.settle_pending(&offline(reason));
            tracing::debug!(worker_fp = %worker_fp, reason, "terminal pipeline cache entry cleared");
        }
    }
}

impl WorkerPipelineCacheEntry {
    pub(super) fn new(entry_id: u64, worker: Arc<WorkerHandle>) -> Self {
        Self {
            entry_id,
            worker,
            cached_error: None,
            expiry_token: 0,
            flush_scheduled: false,
            in_flight: false,
            next_request_at: None,
            pending: Vec::new(),
            request_timer_armed: false,
            targets: HashMap::new(),
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        !self.in_flight && self.pending.is_empty()
    }

    /// The answer the cache alone can give: a live cached error, or every
    /// target's own live sample. `None` when any target needs sampling.
    pub(super) fn project_cached_targets(
        &self,
        targets: &[Target],
        now: Instant,
    ) -> Option<SampleResult> {
        if let Some((error, expires_at)) = &self.cached_error
            && *expires_at > now
        {
            return Some(error.clone());
        }
        let samples: Vec<&CachedTargetSample> = targets
            .iter()
            .map(|target| {
                self.targets
                    .get(target)
                    .filter(|sample| sample.expires_at > now)
            })
            .collect::<Option<_>>()?;
        let first = samples.first()?;
        let sessions: Vec<TerminalPipelineSessionSnapshot> = samples
            .iter()
            .filter_map(|sample| sample.session.clone())
            .collect();
        let dropped_records = u32::try_from(targets.len() - sessions.len()).unwrap_or(u32::MAX);
        let snapshot = WTerminalPipelineSnapshot {
            request_id: first.request_id.clone(),
            dropped_targets: 0,
            dropped_records,
            sessions,
            ..Default::default()
        };
        Some(SampleResult::Ok {
            response_ms: first.response_ms,
            snapshot,
        })
    }

    /// Up to the per-request bound of distinct targets no live sample answers,
    /// in the order the waiting collections asked for them.
    pub(super) fn uncached_batch(&self, now: Instant) -> Vec<Target> {
        let mut seen: HashSet<&Target> = HashSet::new();
        let live = |target: &Target| {
            self.targets
                .get(target)
                .is_some_and(|sample| sample.expires_at > now)
        };
        self.pending
            .iter()
            .flat_map(|collection| collection.targets.iter())
            .filter(|target| !live(target) && seen.insert(target))
            .take(TERMINAL_PIPELINE_DIAG_MAX_TARGETS)
            .cloned()
            .collect()
    }

    pub(super) fn resolve_satisfied_pending(&mut self, now: Instant) {
        let waiting = std::mem::take(&mut self.pending);
        for collection in waiting {
            match self.project_cached_targets(&collection.targets, now) {
                Some(result) => {
                    let _ = collection.reply.send(result);
                }
                None => self.pending.push(collection),
            }
        }
    }

    pub(super) fn settle_pending(&mut self, result: &SampleResult) {
        for collection in self.pending.drain(..) {
            let _ = collection.reply.send(result.clone());
        }
    }

    /// Files each batched target's own session, or `None` when the worker
    /// accounted for it as a dropped record.
    pub(super) fn store_samples(
        &mut self,
        batch: &[Target],
        response_ms: u64,
        snapshot: WTerminalPipelineSnapshot,
        expires_at: Instant,
    ) {
        let mut sessions: HashMap<Target, TerminalPipelineSessionSnapshot> = snapshot
            .sessions
            .into_iter()
            .map(|session| {
                (
                    Target::new(session.session_id.clone(), session.view_id.clone()),
                    session,
                )
            })
            .collect();
        for target in batch {
            let sample = CachedTargetSample {
                expires_at,
                request_id: snapshot.request_id.clone(),
                response_ms,
                session: sessions.remove(target),
            };
            self.targets.insert(target.clone(), sample);
        }
    }
}

pub(super) fn offline(message: &str) -> SampleResult {
    SampleResult::error(0, TerminalPipelineSnapshotErrorCode::Offline, message)
}
