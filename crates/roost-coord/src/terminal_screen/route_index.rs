//! The `(worker, channel) -> session` half of the terminal byte hub.
//!
//! Split out of v2's `byte-hub.ts` (457 lines, over the 400 cap) on the seam
//! `coord_core::seams::WorkerRouteIndex` draws: this module owns the index,
//! the route cache and the retirement bookkeeping, and `byte_hub` owns the
//! cells. Neither names a caller.
//!
//! WHY THE FORWARD INDEX IS NESTED BY WORKER. v2 keeps one `Map` keyed by the
//! string `"<fp>:<channel>"` and prefix-sweeps it. A nested `BTreeMap` makes
//! "every key of this worker" a single subtree, so a replacement is one
//! `BTreeMap` built locally and one swap -- which is what makes
//! [`RouteIndex::replace_worker_channel_index`] atomic rather than a sequence
//! of per-key deletions a concurrent reader could observe halfway through.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use roost_protocol::wire::{ChannelId, SessionId, WorkerFp};

use crate::terminal_screen::unmapped_drop::UnmappedDropDetector;

/// One `(worker, channel)` route that stopped resolving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRetirement {
    /// The worker whose channel stopped carrying that session.
    pub worker_fp: WorkerFp,
    /// The channel on that worker.
    pub channel_id: ChannelId,
    /// The session that route used to name.
    pub session_id: SessionId,
}

/// Told about every route that stops resolving, so the terminal input domain can
/// retire the writes it had queued against it.
pub trait RouteRetirementSink: Send + Sync {
    /// A route is gone. Called once per route, inside the mutation that removed
    /// it, so a caller can order its own cleanup against the index.
    fn route_retired(&self, retirement: &RouteRetirement);
}

/// The sink for a coordinator that has no terminal input domain wired.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRouteRetirement;

impl RouteRetirementSink for NoRouteRetirement {
    fn route_retired(&self, retirement: &RouteRetirement) {
        tracing::debug!(
            worker_fp = %retirement.worker_fp,
            channel_id = retirement.channel_id.as_u32(),
            session_id = %retirement.session_id,
            "a terminal route stopped resolving and no retirement sink is wired"
        );
    }
}

/// A session's cached route, so `SessionsInput` skips a database read per
/// keystroke batch. A stale entry is safe: the worker lookup answers "not
/// routable" and the send is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedRoute {
    pub worker_fp: WorkerFp,
    pub channel_id: ChannelId,
}

/// The index itself, mutated through `&mut self`.
///
/// Its holder owns the lock, which is what lets a replacement be one critical
/// section: a reader either sees the whole old index or the whole new one.
pub struct RouteIndex {
    /// worker -> channel -> session. Nested so one worker's routes are one
    /// value, replaceable without touching any other worker's.
    routes: BTreeMap<WorkerFp, BTreeMap<ChannelId, SessionId>>,
    /// session -> the keys registered for it. Lets a close prune in O(1)
    /// instead of walking the whole forward index, and lets a rebind drop
    /// every key a session used to hold.
    session_to_keys: HashMap<SessionId, HashSet<(WorkerFp, ChannelId)>>,
    /// session -> its last known route, for the keystroke path.
    session_to_worker: HashMap<SessionId, CachedRoute>,
    /// Workers whose exact boot/reconcile snapshot has been applied.
    reconciled: HashSet<WorkerFp>,
    unmapped_drops: UnmappedDropDetector,
    retirement: Arc<dyn RouteRetirementSink>,
}

impl std::fmt::Debug for RouteIndex {
    /// The retirement sink is a trait object; a log line needs the sizes, which
    /// is the question a routing incident is asked.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let routed: usize = self.routes.values().map(BTreeMap::len).sum();
        formatter
            .debug_struct("RouteIndex")
            .field("workers", &self.routes.len())
            .field("routes", &routed)
            .field("reconciled", &self.reconciled.len())
            .finish()
    }
}

impl RouteIndex {
    /// An empty index. The retirement sink is required rather than defaulted
    /// because a route that silently stops resolving is a coordinator bug that
    /// only shows up as a terminal that stopped accepting input.
    #[must_use]
    pub fn new(retirement: Arc<dyn RouteRetirementSink>) -> Self {
        Self {
            routes: BTreeMap::new(),
            session_to_keys: HashMap::new(),
            session_to_worker: HashMap::new(),
            reconciled: HashSet::new(),
            unmapped_drops: UnmappedDropDetector::new(),
            retirement,
        }
    }

    /// How many workers currently carry at least one route, for a log line.
    #[must_use]
    pub fn worker_route_count(&self) -> usize {
        self.routes.len()
    }

    /// The session a worker's channel currently carries.
    #[must_use]
    pub fn resolve(&self, worker_fp: &WorkerFp, channel_id: ChannelId) -> Option<SessionId> {
        self.routes.get(worker_fp)?.get(&channel_id).cloned()
    }

    /// Every route this worker currently carries, in channel order.
    #[must_use]
    pub fn worker_routes(&self, worker_fp: &WorkerFp) -> BTreeMap<ChannelId, SessionId> {
        self.routes.get(worker_fp).cloned().unwrap_or_default()
    }

    /// Whether a worker's exact snapshot has been applied. Before it, the index
    /// is legitimately incomplete; after it, a session with no live route is
    /// offline and must not be resurrected from a database breadcrumb.
    #[must_use]
    pub fn is_reconciled(&self, worker_fp: &WorkerFp) -> bool {
        self.reconciled.contains(worker_fp)
    }

    /// A fresh authenticated connection reopens the pre-reconcile window: its
    /// hello re-primes the index and its snapshot has not landed yet.
    pub fn reset_reconcile(&mut self, worker_fp: &WorkerFp) {
        self.reconciled.remove(worker_fp);
    }

    /// Remember a session's route, for the keystroke path.
    pub fn cache_route(&mut self, session_id: &SessionId, route: CachedRoute) {
        self.session_to_worker.insert(session_id.clone(), route);
    }

    /// The route a session last resolved to, if the index has one.
    #[must_use]
    pub fn cached_route(&self, session_id: &SessionId) -> Option<&CachedRoute> {
        self.session_to_worker.get(session_id)
    }

    /// Forget a session's cached route and its last-cell record's owner.
    pub fn evict_route(&mut self, session_id: &SessionId) {
        self.session_to_worker.remove(session_id);
    }

    /// Bind one key, dropping whatever it used to name.
    ///
    /// A channel may move between sessions only after the prior parser state
    /// has retired; otherwise a split legacy OSC sequence crosses the binding.
    pub fn bind(&mut self, worker_fp: &WorkerFp, channel_id: ChannelId, session_id: &SessionId) {
        self.forget_key(worker_fp, channel_id);
        self.routes
            .entry(worker_fp.clone())
            .or_default()
            .insert(channel_id, session_id.clone());
        self.session_to_keys
            .entry(session_id.clone())
            .or_default()
            .insert((worker_fp.clone(), channel_id));
        self.unmapped_drops.clear(worker_fp, channel_id);
    }

    /// Prime from the durable rows on a worker `hello`, so a coordinator that
    /// restarted under a live worker does not drop its bytes until the next
    /// snapshot.
    pub fn prime(&mut self, rows: &[(SessionId, WorkerFp, ChannelId)]) {
        for (session_id, worker_fp, channel_id) in rows {
            self.bind(worker_fp, *channel_id, session_id);
            self.cache_route(
                session_id,
                CachedRoute {
                    worker_fp: worker_fp.clone(),
                    channel_id: *channel_id,
                },
            );
        }
    }

    /// `respawned`: the keeper handed this session a NEW channel on `worker_fp`
    /// -- the fingerprint that authenticated the connection the event arrived
    /// on, never a cached route, which can hold an arbitrarily old one.
    pub fn rebind_respawned(
        &mut self,
        worker_fp: &WorkerFp,
        session_id: &SessionId,
        new_channel: ChannelId,
    ) {
        self.unbind_session_keys(session_id);
        self.bind(worker_fp, new_channel, session_id);
        self.cache_route(
            session_id,
            CachedRoute {
                worker_fp: worker_fp.clone(),
                channel_id: new_channel,
            },
        );
    }

    /// Forget one route: the forward key, its reverse-index entry, and its
    /// unmapped-drop window.
    pub fn forget_key(&mut self, worker_fp: &WorkerFp, channel_id: ChannelId) {
        let Some(sessions) = self.routes.get_mut(worker_fp) else {
            return;
        };
        if let Some(session_id) = sessions.remove(&channel_id) {
            if let Some(keys) = self.session_to_keys.get_mut(&session_id) {
                keys.remove(&(worker_fp.clone(), channel_id));
                if keys.is_empty() {
                    self.session_to_keys.remove(&session_id);
                }
            }
            self.retirement.route_retired(&RouteRetirement {
                worker_fp: worker_fp.clone(),
                channel_id,
                session_id,
            });
        }
        if sessions.is_empty() {
            self.routes.remove(worker_fp);
        }
        self.unmapped_drops.clear(worker_fp, channel_id);
    }

    /// Drop every route a session was bound under, so a rebind cannot leave the
    /// old channel resolving to it.
    pub fn unbind_session_keys(&mut self, session_id: &SessionId) {
        let Some(keys) = self.session_to_keys.get(session_id).cloned() else {
            return;
        };
        for (worker_fp, channel_id) in keys {
            self.forget_key(&worker_fp, channel_id);
        }
    }

    /// Make `live` this worker's ENTIRE channel index, in one step.
    ///
    /// Every key of this worker that is not in `live` is dropped, every route
    /// cache entry of a session the worker no longer runs is evicted, every
    /// session in `live` is rebound away from any older key it carried, and
    /// the worker is marked reconciled -- all inside the caller's single
    /// critical section, so a concurrent reader sees one index or the other.
    pub fn replace_worker_channel_index(
        &mut self,
        worker_fp: &WorkerFp,
        live: &[(SessionId, ChannelId)],
    ) {
        let live_sessions: HashSet<&SessionId> = live.iter().map(|(id, _)| id).collect();
        let mut next: BTreeMap<ChannelId, SessionId> = BTreeMap::new();
        for (session_id, channel_id) in live {
            next.insert(*channel_id, session_id.clone());
        }
        // Built before anything is removed, so the sweep below cannot observe
        // a key that is about to exist.
        let superseded: Vec<(WorkerFp, ChannelId)> = self
            .session_to_keys
            .iter()
            .filter(|(session_id, _)| live_sessions.contains(session_id))
            .flat_map(|(_, keys)| keys.iter().cloned())
            .filter(|(fp, channel)| *fp != *worker_fp || !next.contains_key(channel))
            .collect();
        for (fp, channel_id) in superseded {
            self.forget_key(&fp, channel_id);
        }
        if let Some(previous) = self.routes.remove(worker_fp) {
            for (channel_id, session_id) in previous {
                if !next.contains_key(&channel_id) {
                    self.retirement.route_retired(&RouteRetirement {
                        worker_fp: worker_fp.clone(),
                        channel_id,
                        session_id,
                    });
                }
            }
        }
        if !next.is_empty() {
            self.routes.insert(worker_fp.clone(), next);
        }
        for (session_id, channel_id) in live {
            self.session_to_keys
                .entry(session_id.clone())
                .or_default()
                .insert((worker_fp.clone(), *channel_id));
            self.unmapped_drops.clear(worker_fp, *channel_id);
        }
        // A route-cache entry can exist with no channel key of its own -- the
        // pre-reconcile database fallback caches one -- so the cache needs its
        // own sweep, and it is keyed by worker rather than by prefix.
        self.session_to_worker.retain(|session_id, route| {
            route.worker_fp != *worker_fp || live_sessions.contains(session_id)
        });
        for (session_id, channel_id) in live {
            self.cache_route(
                session_id,
                CachedRoute {
                    worker_fp: worker_fp.clone(),
                    channel_id: *channel_id,
                },
            );
        }
        self.reconciled.insert(worker_fp.clone());
    }

    /// Permanently retire every volatile route of a deleted worker, keeping its
    /// durable breadcrumbs, and name the sessions that just lost one.
    pub fn retire_worker_routes(&mut self, worker_fp: &WorkerFp) -> Vec<SessionId> {
        let mut affected: Vec<SessionId> = self
            .session_to_worker
            .iter()
            .filter(|(_, route)| route.worker_fp == *worker_fp)
            .map(|(session_id, _)| session_id.clone())
            .collect();
        if let Some(sessions) = self.routes.get(worker_fp) {
            for session_id in sessions.values() {
                if !affected.contains(session_id) {
                    affected.push(session_id.clone());
                }
            }
        }
        self.replace_worker_channel_index(worker_fp, &[]);
        affected
    }

    /// Record a frame that arrived on a channel nothing resolves to.
    pub fn record_unmapped_drop(
        &mut self,
        worker_fp: &WorkerFp,
        channel_id: ChannelId,
        now_ms: i64,
    ) {
        self.unmapped_drops.record(worker_fp, channel_id, now_ms);
    }

    /// A channel that just bound or published is no longer dropping.
    pub fn clear_unmapped_drop(&mut self, worker_fp: &WorkerFp, channel_id: ChannelId) {
        self.unmapped_drops.clear(worker_fp, channel_id);
    }

    /// How many channels the drop detector is currently watching.
    #[must_use]
    pub fn watched_unmapped_channels(&self) -> usize {
        self.unmapped_drops.watched_channels()
    }
}
