//! The cell half of the terminal byte hub: the frames that arrive on a route.
//!
//! Split out of v2's `byte-hub.ts` (457 lines, over the 400 cap) on the seam
//! `coord_core::seams::WorkerRouteIndex` draws. `route_index` owns the
//! `(worker, channel) -> session` mapping; this file owns what happens to a
//! cell frame once that mapping has answered. Neither names a caller.
//!
//! v2 kept six module-level `Map`s. They are fields here, which is why this
//! domain has no crate-global: `services.rs` states the rule, and a test that
//! constructed its own hub would otherwise see another test's routes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use roost_proto::{PbCellGridChunk, PbCellGridFrame};
use roost_protocol::wire::{ChannelId, SessionId, WorkerFp};

use crate::coord_core::seams::{LiveChannel, WorkerRouteIndex};
use crate::terminal_screen::replica::ScreenHub;
use crate::terminal_screen::route_index::{CachedRoute, RouteIndex, RouteRetirementSink};

/// The last cell a session's worker sent, for a diagnostic request.
///
/// One mutable record per live session, updated in place on the hot path; a
/// diagnostic request copies it rather than the hot path allocating one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastCellDiagnostic {
    pub worker_fp: WorkerFp,
    pub channel_id: ChannelId,
    pub grid_epoch: String,
    pub seq: u64,
    pub full: bool,
    pub received_at_ms: i64,
}

/// Why a frame did not reach the screen replica.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The replica took it.
    Published { session_id: SessionId },
    /// Nothing resolves this `(worker, channel)`, so the frame was dropped.
    DroppedUnmapped,
    /// The frame named a session the route does not carry, so the replica was
    /// invalidated rather than fed a frame belonging to someone else.
    DroppedMismatchedSession { session_id: SessionId },
    /// Nothing expects a stream for that session yet.
    NoExpectedStream { session_id: SessionId },
}

/// The per-session terminal byte hub.
///
/// `Clone` shares: the clone is another handle on the same routes, cells and
/// screen replicas, which is what makes `Arc<ByteHub>` the right way to hand
/// it to the workers domain.
#[derive(Clone)]
pub struct ByteHub {
    routes: Arc<Mutex<RouteIndex>>,
    cells: Arc<Mutex<HashMap<SessionId, LastCellDiagnostic>>>,
    screens: Arc<ScreenHub>,
}

impl std::fmt::Debug for ByteHub {
    /// The hub holds three locks and a replica map; a log line needs the sizes
    /// it would change, not the frames.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let route_count = self
            .routes
            .lock()
            .map(|routes| routes.worker_route_count())
            .unwrap_or_default();
        formatter
            .debug_struct("ByteHub")
            .field("worker_routes", &route_count)
            .finish_non_exhaustive()
    }
}

impl ByteHub {
    /// A hub with no routes and no replicas.
    #[must_use]
    pub fn new(screens: Arc<ScreenHub>, retirement: Arc<dyn RouteRetirementSink>) -> Self {
        Self {
            routes: Arc::new(Mutex::new(RouteIndex::new(retirement))),
            cells: Arc::new(Mutex::new(HashMap::new())),
            screens,
        }
    }

    /// A hub with the hard residency maxima, for a coordinator that declared no
    /// byte budget.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(
            Arc::new(ScreenHub::new()),
            Arc::new(crate::terminal_screen::route_index::NoRouteRetirement),
        )
    }

    /// The screen replicas this hub feeds.
    #[must_use]
    pub fn screens(&self) -> &Arc<ScreenHub> {
        &self.screens
    }

    /// The session a worker's channel carries, if the index has it.
    #[must_use]
    pub fn resolve(&self, worker_fp: &WorkerFp, channel_id: ChannelId) -> Option<SessionId> {
        self.routes.lock().ok()?.resolve(worker_fp, channel_id)
    }

    /// The last cell a session's worker sent, for a diagnostic request.
    #[must_use]
    pub fn last_cell(&self, session_id: &SessionId) -> Option<LastCellDiagnostic> {
        self.cells.lock().ok()?.get(session_id).cloned()
    }

    /// Every route this worker currently carries, read under ONE lock.
    ///
    /// Two `resolve` calls are two reads and can straddle a replacement, which
    /// is correct -- a replacement is atomic per lookup, because the trait
    /// answers one key per call. Reading a worker's WHOLE index at once is the
    /// only read that can be checked against a whole-index replacement, and it
    /// is what a caller reconciling against a snapshot needs.
    #[must_use]
    pub fn worker_routes(
        &self,
        worker_fp: &WorkerFp,
    ) -> std::collections::BTreeMap<ChannelId, SessionId> {
        self.routes
            .lock()
            .map(|routes| routes.worker_routes(worker_fp))
            .unwrap_or_default()
    }

    /// The route a session last resolved to, for the keystroke path.
    #[must_use]
    pub fn cached_route(&self, session_id: &SessionId) -> Option<CachedRoute> {
        self.routes.lock().ok()?.cached_route(session_id).cloned()
    }

    /// Forget a session's cached route AND its last-cell record.
    ///
    /// Both, because a stale last-cell record advertises a channel the worker
    /// no longer owns until the next frame lands.
    pub fn evict_session(&self, session_id: &SessionId) {
        if let Ok(mut routes) = self.routes.lock() {
            routes.evict_route(session_id);
        }
        if let Ok(mut cells) = self.cells.lock() {
            cells.remove(session_id);
        }
    }

    /// Prime the index from durable rows on a worker `hello`.
    pub fn prime_channel_map(&self, rows: &[(SessionId, WorkerFp, ChannelId)]) {
        if let Ok(mut routes) = self.routes.lock() {
            routes.prime(rows);
        }
    }

    /// Whether a worker's exact boot/reconcile snapshot has been applied.
    #[must_use]
    pub fn is_worker_channel_index_reconciled(&self, worker_fp: &WorkerFp) -> bool {
        self.routes
            .lock()
            .map(|routes| routes.is_reconciled(worker_fp))
            .unwrap_or(false)
    }

    /// Reopen the pre-reconcile window for a newly authenticated connection.
    pub fn reset_worker_channel_index_reconcile(&self, worker_fp: &WorkerFp) {
        if let Ok(mut routes) = self.routes.lock() {
            routes.reset_reconcile(worker_fp);
        }
    }

    /// `respawned`: rebind a session to the channel its keeper just handed it,
    /// and drop the stale last-cell record, whose channel no longer exists.
    pub fn rebind_respawned_channel(
        &self,
        worker_fp: &WorkerFp,
        session_id: &SessionId,
        new_channel: ChannelId,
    ) {
        if let Ok(mut routes) = self.routes.lock() {
            routes.rebind_respawned(worker_fp, session_id, new_channel);
        }
        if let Ok(mut cells) = self.cells.lock() {
            cells.remove(session_id);
        }
    }

    /// Deliver one whole cell frame, stamping the durable session binding onto
    /// it. A worker frame carries a `channel_id` only.
    pub fn publish_cell_grid(
        &self,
        worker_fp: &WorkerFp,
        channel_id: ChannelId,
        frame: &mut PbCellGridFrame,
        now_ms: i64,
    ) -> PublishOutcome {
        let Some(session_id) = self.resolve(worker_fp, channel_id) else {
            tracing::debug!(
                worker_fp = %worker_fp,
                channel_id = channel_id.as_u32(),
                "a cell frame arrived on an unbound channel and was dropped"
            );
            self.record_unmapped_drop(worker_fp, channel_id, now_ms);
            return PublishOutcome::DroppedUnmapped;
        };
        if !frame.session_id.is_empty() && frame.session_id != session_id.as_str() {
            tracing::warn!(
                worker_fp = %worker_fp,
                channel_id = channel_id.as_u32(),
                expected_session_id = %session_id,
                received_session_id = %frame.session_id,
                "a worker cell frame carried a session id the route does not carry"
            );
            self.screens.invalidate(
                &session_id,
                "worker cell frame carried a mismatched session id",
            );
            return PublishOutcome::DroppedMismatchedSession { session_id };
        }
        self.clear_unmapped_drop(worker_fp, channel_id);
        frame.session_id = session_id.as_str().to_owned();
        frame.coord_recv_ms = now_ms.max(0) as u64;
        self.record_last_cell(&session_id, worker_fp, channel_id, frame, now_ms);
        self.screens.publish_frame(&session_id, frame, now_ms)
    }

    /// Deliver one chunk of a chunked baseline, the multipart form of the same
    /// relay. The chunk's own `part` names the session, when it carries one.
    pub fn publish_cell_grid_chunk(
        &self,
        worker_fp: &WorkerFp,
        channel_id: ChannelId,
        chunk: &mut PbCellGridChunk,
        now_ms: i64,
    ) -> PublishOutcome {
        let Some(session_id) = self.resolve(worker_fp, channel_id) else {
            tracing::debug!(
                worker_fp = %worker_fp,
                channel_id = channel_id.as_u32(),
                "a cell chunk arrived on an unbound channel and was dropped"
            );
            self.record_unmapped_drop(worker_fp, channel_id, now_ms);
            return PublishOutcome::DroppedUnmapped;
        };
        if let Some(part) = chunk.part.as_option()
            && !part.session_id.is_empty()
            && part.session_id != session_id.as_str()
        {
            tracing::warn!(
                worker_fp = %worker_fp,
                channel_id = channel_id.as_u32(),
                expected_session_id = %session_id,
                received_session_id = %part.session_id,
                "a worker cell chunk carried a session id the route does not carry"
            );
            self.screens.invalidate(
                &session_id,
                "worker cell chunk carried a mismatched session id",
            );
            return PublishOutcome::DroppedMismatchedSession { session_id };
        }
        self.clear_unmapped_drop(worker_fp, channel_id);
        if let Some(part) = chunk.part.as_option_mut() {
            part.session_id = session_id.as_str().to_owned();
        }
        self.screens.publish_chunk(&session_id, chunk, now_ms)
    }

    fn record_unmapped_drop(&self, worker_fp: &WorkerFp, channel_id: ChannelId, now_ms: i64) {
        if let Ok(mut routes) = self.routes.lock() {
            routes.record_unmapped_drop(worker_fp, channel_id, now_ms);
        }
    }

    fn clear_unmapped_drop(&self, worker_fp: &WorkerFp, channel_id: ChannelId) {
        if let Ok(mut routes) = self.routes.lock() {
            routes.clear_unmapped_drop(worker_fp, channel_id);
        }
    }

    fn record_last_cell(
        &self,
        session_id: &SessionId,
        worker_fp: &WorkerFp,
        channel_id: ChannelId,
        frame: &PbCellGridFrame,
        now_ms: i64,
    ) {
        let Ok(mut cells) = self.cells.lock() else {
            return;
        };
        let record = LastCellDiagnostic {
            worker_fp: worker_fp.clone(),
            channel_id,
            grid_epoch: frame.grid_epoch.clone(),
            seq: frame.seq,
            full: frame.full,
            received_at_ms: now_ms,
        };
        match cells.get_mut(session_id) {
            Some(existing) => *existing = record,
            None => {
                cells.insert(session_id.clone(), record);
            }
        }
    }
}

impl WorkerRouteIndex for ByteHub {
    fn lookup_session_id(&self, worker_fp: &WorkerFp, channel_id: &ChannelId) -> Option<SessionId> {
        self.resolve(worker_fp, *channel_id)
    }

    fn replace_worker_channel_index(&self, worker_fp: &WorkerFp, live: &[LiveChannel]) {
        let entries: Vec<(SessionId, ChannelId)> = live
            .iter()
            .map(|entry| (entry.session_id.clone(), entry.channel_id))
            .collect();
        if let Ok(mut routes) = self.routes.lock() {
            routes.replace_worker_channel_index(worker_fp, &entries);
        }
    }

    fn retire_worker_routes(&self, worker_fp: &WorkerFp) -> Vec<SessionId> {
        self.routes
            .lock()
            .map(|mut routes| routes.retire_worker_routes(worker_fp))
            .unwrap_or_default()
    }
}
