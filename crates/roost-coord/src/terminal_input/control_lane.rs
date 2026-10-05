//! The terminal-input admission substrate: the tab-scoped sender identity, the
//! bounded per-sender/session FIFO, Sync-generation cancellation, and the live
//! session-route resolution every write takes inside that FIFO. View membership
//! and SCD never enter this lane; they are the terminal view hub's.
//! Owned by `TerminalInputRuntime`; entered by `terminal_input::write_control`.
//! Ports `apps/coord/src/terminal/input/terminal-control-lane.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_protocol::wire::{ChannelId, SessionId, WorkerFp};
use tokio::sync::oneshot;

use crate::db::CoordDb;
use crate::terminal_screen::byte_hub::ByteHub;
use crate::terminal_screen::route_index::CachedRoute;

/// Commands one sender may queue against one session.
pub const MAX_LANE_DEPTH: usize = 256;

/// Commands every sender together may queue.
pub const MAX_AGGREGATE_DEPTH: usize = 2_048;

/// Who is writing: the stable lane key and the fingerprint the audit names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalViewerIdentity {
    /// `${fingerprint}:${tab}`, or the bare fingerprint without a tab.
    pub viewer_key: String,
    /// The authenticated device fingerprint.
    pub caller_fingerprint: String,
}

/// The sender identity unary input and Sync input share. The remote address is
/// deliberately not part of the key: a tailnet address change must not mint a
/// second sender and reorder one tab's keystrokes.
#[must_use]
pub fn terminal_viewer_identity(
    caller_fingerprint: &str,
    tab_id: Option<&str>,
) -> TerminalViewerIdentity {
    let viewer_key = match tab_id {
        Some(tab_id) if !tab_id.is_empty() => format!("{caller_fingerprint}:{tab_id}"),
        _ => caller_fingerprint.to_owned(),
    };
    TerminalViewerIdentity {
        viewer_key,
        caller_fingerprint: caller_fingerprint.to_owned(),
    }
}

/// Every sender/session lane, and the Sync generations with commands queued.
#[derive(Debug, Default)]
pub struct ControlLanes {
    table: Mutex<LaneTable>,
}

#[derive(Debug, Default)]
struct LaneTable {
    lanes: HashMap<(String, String), Lane>,
    generations: HashMap<(String, String), GenerationQueue>,
    aggregate_depth: usize,
}

/// One sender/session FIFO. `tail` resolves when the newest command releases
/// the lane or settles, whichever comes first.
#[derive(Debug, Default)]
struct Lane {
    depth: usize,
    tail: Option<oneshot::Receiver<()>>,
}

#[derive(Debug, Default)]
struct GenerationQueue {
    queued: usize,
    canceled: bool,
}

/// One command's place in its lane, held until the command settles: depth
/// counts through settlement, not only until the lane is released.
#[derive(Debug)]
pub struct LaneTicket {
    lanes: Arc<ControlLanes>,
    lane_key: (String, String),
    generation_key: Option<(String, String)>,
    predecessor: Option<oneshot::Receiver<()>>,
    handoff: Option<oneshot::Sender<()>>,
}

impl ControlLanes {
    /// No lanes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a place behind the sender's previous command for this session, or
    /// `None` when the lane or the aggregate is full. Synchronous, so the
    /// order commands enter is the order their socket delivered them.
    pub fn enqueue(
        self: &Arc<Self>,
        viewer_key: &str,
        session_id: &str,
        socket_generation: Option<&str>,
    ) -> Option<LaneTicket> {
        let mut table = self.table();
        let lane_key = (viewer_key.to_owned(), session_id.to_owned());
        let depth = table.lanes.get(&lane_key).map_or(0, |lane| lane.depth);
        if depth >= MAX_LANE_DEPTH || table.aggregate_depth >= MAX_AGGREGATE_DEPTH {
            return None;
        }
        table.aggregate_depth += 1;
        let (handoff, next_tail) = oneshot::channel();
        let lane = table.lanes.entry(lane_key.clone()).or_default();
        lane.depth += 1;
        let predecessor = lane.tail.replace(next_tail);
        let generation_key = socket_generation
            .filter(|generation| !generation.is_empty())
            .map(|generation| (viewer_key.to_owned(), generation.to_owned()));
        if let Some(key) = &generation_key {
            table.generations.entry(key.clone()).or_default().queued += 1;
        }
        Some(LaneTicket {
            lanes: Arc::clone(self),
            lane_key,
            generation_key,
            predecessor,
            handoff: Some(handoff),
        })
    }

    /// Cancel the commands a closing Sync generation queued that have not
    /// begun. A command already running keeps running, and the lane tail makes
    /// the replacement generation's first command wait behind it.
    pub fn cancel_generation(&self, viewer_key: &str, socket_generation: &str) {
        if socket_generation.is_empty() {
            return;
        }
        let key = (viewer_key.to_owned(), socket_generation.to_owned());
        if let Some(queue) = self.table().generations.get_mut(&key) {
            queue.canceled = true;
            tracing::info!(
                viewer_key,
                socket_generation,
                queued = queue.queued,
                "queued terminal input cancelled with its Sync generation"
            );
        }
    }

    fn settle(&self, lane_key: &(String, String), generation_key: Option<&(String, String)>) {
        let mut table = self.table();
        table.aggregate_depth = table.aggregate_depth.saturating_sub(1);
        if let Some(lane) = table.lanes.get_mut(lane_key) {
            lane.depth = lane.depth.saturating_sub(1);
            if lane.depth == 0 {
                table.lanes.remove(lane_key);
            }
        }
        if let Some(key) = generation_key
            && let Some(queue) = table.generations.get_mut(key)
        {
            queue.queued = queue.queued.saturating_sub(1);
            if queue.queued == 0 {
                table.generations.remove(key);
            }
        }
    }

    fn generation_canceled(&self, generation_key: Option<&(String, String)>) -> bool {
        generation_key.is_some_and(|key| {
            self.table()
                .generations
                .get(key)
                .is_some_and(|queue| queue.canceled)
        })
    }

    fn table(&self) -> MutexGuard<'_, LaneTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl LaneTicket {
    /// Wait for the command ahead to release the lane. `false` when this
    /// command's Sync generation closed before it began.
    pub async fn wait_turn(&mut self) -> bool {
        if let Some(predecessor) = self.predecessor.take() {
            // A dropped sender is a settled predecessor: either way it is done
            // with the lane.
            let _ = predecessor.await;
        }
        !self.lanes.generation_canceled(self.generation_key.as_ref())
    }

    /// Let the next queued command start while this one keeps finalizing.
    pub fn release_lane(&mut self) {
        self.handoff.take();
    }
}

impl Drop for LaneTicket {
    /// A command that never releases still gates the lane on its own
    /// settlement. One abandoned before its turn hands its successor the wait
    /// it never finished, so no ordering is lost.
    fn drop(&mut self) {
        let handoff = self.handoff.take();
        if let (Some(predecessor), Some(handoff)) = (self.predecessor.take(), handoff)
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move {
                let _ = predecessor.await;
                drop(handoff);
            });
        }
        self.lanes
            .settle(&self.lane_key, self.generation_key.as_ref());
    }
}

/// The live route an input command may use: the byte hub's cached route when
/// it holds one, else the durable open session on a non-deleted worker.
///
/// A cache hit skips the database because an entry only exists through
/// `admit_durable_route` (a proven open row), `prime` (a worker's hello rows),
/// a committed `opened`/`respawned`/`snapshot` event, and leaves through
/// `evict_route` (a committed `closed` event), `retire_worker_routes` (a
/// deleted worker) or the reconcile sweep of a worker's live set. A miss
/// proves the durable relationship first, so a guessed session id cannot
/// create an entry; once a worker has announced its exact live set, a session
/// absent from it is offline and is not re-cached from the row.
pub async fn resolve_session_route(
    db: &CoordDb,
    byte_hub: &ByteHub,
    session_id: &str,
) -> Result<Option<CachedRoute>, sqlx::Error> {
    let Ok(session) = SessionId::try_from(session_id) else {
        return Ok(None);
    };
    if let Some(route) = byte_hub.cached_route(&session) {
        return Ok(Some(route));
    }
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT session.worker_fp, session.channel FROM sessions AS session \
         INNER JOIN workers AS worker ON worker.fp = session.worker_fp \
         WHERE session.id = ?1 AND session.status = 'open' AND worker.deleted_at_ms IS NULL",
    )
    .bind(session_id)
    .fetch_optional(db.pool())
    .await?;
    let Some((worker_fp, channel)) = row else {
        return Ok(None);
    };
    let (Ok(worker_fp), Ok(channel_id)) =
        (WorkerFp::try_from(worker_fp), ChannelId::try_from(channel))
    else {
        tracing::warn!(session_id, "an open session row names no addressable route");
        return Ok(None);
    };
    Ok(byte_hub.admit_durable_route(
        &session,
        CachedRoute {
            worker_fp,
            channel_id,
        },
    ))
}
