// The two seams the workers domain needs from the terminal domain.
//
// WHY TRAITS, AND NOT A DEPENDENCY RULE. A `use` cycle between two modules of
// ONE crate compiles in Rust -- there is no E0403 for sibling modules, and
// `xtask lint`'s dependency DAG is crate-level, so it would not catch a
// `workers <-> terminal` cycle either. Nothing here is forced by the compiler.
// The reason to break the cycle is the one
// `sync_ws/terminal/snapshot.rs` already states: the CONSUMER must be testable
// without the collaborator, and a constructor parameter is what buys that. A
// workers handler that names `ByteHub` directly cannot be exercised without one.
//
// TWO TRAITS, NOT ONE, AND THE REASON IS CALLEE IDENTITY. `retire_worker_routes`
// is answered by the byte hub and `notify_worker_retired` by the view hub:
// separate objects, separate lifetimes, which merely happen to be called back to
// back. Folding them into one trait would require the byte hub to hold a
// view-hub reference to answer the second, which relocates the cycle INSIDE
// terminal rather than removing it, and hands the workers slice a god object.
//
// The OTHER direction, terminal -> workers, is NOT here on purpose.
// `connectWorkers` is a live transport map, not a service, and its handles are
// replaced on every hello and reconnect. A `&dyn` cannot express that; it is
// shared state, so it lives on `CoordServices` as a registry instead.

use std::sync::Arc;

use roost_protocol::viewport::TerminalGeometry;
use roost_protocol::wire::{ChannelId, SessionId, WorkerFp};

use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};

/// One channel a worker just announced as live, paired with the session that
/// owns it. A `Vec<LiveChannel>` is a whole generation's channel index for one
/// worker: the reconciler's unit of truth, not a delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveChannel {
    /// The session the channel belongs to.
    pub session_id: SessionId,
    /// The channel on that session.
    pub channel_id: ChannelId,
}

/// The terminal byte hub's route cache, as the workers domain needs it.
///
/// Owned by the terminal domain's byte hub. The workers connect path, the
/// delete handler and the WebSocket handshake call it at a hello, a delete and
/// a frame dispatch. An empty index is a real state meaning no live route, not
/// an error.
pub trait WorkerRouteIndex: Send + Sync {
    /// The session a worker's channel currently carries, if the index has it.
    fn lookup_session_id(&self, worker_fp: &WorkerFp, channel_id: &ChannelId) -> Option<SessionId>;

    /// Atomically make `live` this worker's entire channel index, dropping every
    /// key and route-cache entry of that worker not in it.
    fn replace_worker_channel_index(&self, worker_fp: &WorkerFp, live: &[LiveChannel]);

    /// Drop every volatile route of a deleted worker, and name the sessions
    /// that just lost one so their cleanup can fail independently of the
    /// retirement itself.
    fn retire_worker_routes(&self, worker_fp: &WorkerFp) -> Vec<SessionId>;
}

/// The terminal view hub's lifecycle notifications, as the workers domain needs
/// them.
///
/// Owned by the terminal domain's view hub, and it depends on that hub's own
/// membership and stream controller -- never on the workers domain. `None`
/// means nothing is watching that session, which is the common case and the
/// reason a respawn falls back to a default geometry.
pub trait TerminalViewLifecycle: Send + Sync {
    /// A worker is gone; park or drop its view memberships for these sessions.
    fn notify_worker_retired(&self, worker_fp: &WorkerFp, session_ids: &[SessionId]);

    /// The geometry the session's current effective viewer set produced.
    ///
    /// Deliberately narrower than v2's `terminalViewSnapshot`: the respawn
    /// dispatcher reads `.effective` and nothing else, and the full snapshot
    /// also carries active views, parked views, a stream id and an unavailable
    /// flag, of which four of the five it would ignore.
    fn effective_geometry(&self, session_id: &SessionId) -> Option<TerminalGeometry>;
}

/// What a caller without a terminal collaborator gets.
///
/// A unit struct rather than a separate no-op per trait: a test that wants "no
/// terminal" wants both, and two no-op types invite a caller to wire one and
/// forget the other. The registry is the exception and has no no-op at all --
/// an empty `WorkerRegistry` is the real "no workers", which is strictly better
/// than a trait impl that pretends.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTerminalSeams;

impl WorkerRouteIndex for NoTerminalSeams {
    fn lookup_session_id(
        &self,
        _worker_fp: &WorkerFp,
        _channel_id: &ChannelId,
    ) -> Option<SessionId> {
        None
    }

    fn replace_worker_channel_index(&self, _worker_fp: &WorkerFp, _live: &[LiveChannel]) {}

    fn retire_worker_routes(&self, _worker_fp: &WorkerFp) -> Vec<SessionId> {
        Vec::new()
    }
}

impl TerminalViewLifecycle for NoTerminalSeams {
    fn notify_worker_retired(&self, _worker_fp: &WorkerFp, _session_ids: &[SessionId]) {}

    fn effective_geometry(&self, _session_id: &SessionId) -> Option<TerminalGeometry> {
        None
    }
}

/// The terminal seams, held the way `CoordCore` holds everything else: behind
/// traits, so `coord_core` never names a terminal type.
#[derive(Clone)]
pub struct CoordTerminal {
    /// The byte hub's route cache, as the workers domain sees it.
    pub routes: Arc<dyn WorkerRouteIndex>,
    /// The view hub's lifecycle, as the workers domain sees it.
    pub views: Arc<dyn TerminalViewLifecycle>,
}

impl std::fmt::Debug for CoordTerminal {
    /// The collaborators are traits, and what a log line needs to know is
    /// WHICH are wired, not what they contain.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoordTerminal")
            .field("routes", &std::any::type_name::<Self>())
            .field("views", &std::any::type_name::<Self>())
            .finish()
    }
}

impl Default for CoordTerminal {
    fn default() -> Self {
        Self::none()
    }
}

impl CoordTerminal {
    /// The seams for a coordinator with no terminal collaborator.
    #[must_use]
    pub fn none() -> Self {
        let none = Arc::new(NoTerminalSeams);
        Self {
            routes: Arc::clone(&none) as Arc<dyn WorkerRouteIndex>,
            views: none as Arc<dyn TerminalViewLifecycle>,
        }
    }

    /// The seams a real terminal hub provides.
    #[must_use]
    pub fn new(routes: Arc<dyn WorkerRouteIndex>, views: Arc<dyn TerminalViewLifecycle>) -> Self {
        Self { routes, views }
    }
}

/// Re-exported so a caller that holds a route index does not need the type.
impl CoordTerminal {
    /// The worker a terminal send should target, if one is routable.
    ///
    /// A convenience over the registry, so the terminal domain does not have to
    /// know how the workers domain stores handles.
    #[must_use]
    pub fn routable_worker(
        &self,
        registry: &WorkerRegistry,
        worker_fp: &WorkerFp,
    ) -> Option<Arc<WorkerHandle>> {
        registry.current_routable(worker_fp)
    }
}
