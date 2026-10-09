//! The retained seeds a Sync socket gets: the v1 burst of retained snapshots at
//! open, and the v2 per-domain replay a `domain_ready` asks for.
//!
//! Called by `sync_ws::socket_open` (the v1 seed, before any live frame) and by
//! `sync_ws::socket` for a v2 `domain_ready` (`IngressEffect::SeedDomain`).
//! Ports `retainedSeedFrames` and `seedDomain` of
//! `apps/coord/src/sync/sync-feed-seed.ts` and the seed branch of `startSyncFeed`
//! in `sync-feed.ts`.
//!
//! THE SOURCES ARE READ UNLOCKED AND FILTERED UNDER THE LINK. Each retained
//! owner (titles, activity, agent status, viewers, routability) has its own
//! lock; taking them while this socket's link is held would order two locks
//! against each other that the bus listeners take the other way round. The
//! socket's scope is read under the link, where the live feed also moves it.

use std::collections::{BTreeMap, BTreeSet};

use roost_proto::SyncDomain;
use roost_protocol::viewport::TerminalGeometry;
use roost_protocol::wire::{AgentStatus, AgentStatusUpdate, WorkerFp};
use serde_json::json;
use tokio::sync::oneshot;

use crate::events::bus_messages::{
    LastActivityUpdate, SessionPresenceUpdate, SessionTitleUpdate, WorkerRoutableSet,
};
use crate::services::CoordServices;
use crate::sync_ws::driver::{LinkState, SyncLink, now_ms};
use crate::sync_ws::feed::FeedFrame;
use crate::sync_ws::feed::frames::{agent_status_frame, session_title_frame};
use crate::sync_ws::feed::last_activity::last_activity_frame;
use crate::sync_ws::feed::presence::session_presence_frame;
use crate::sync_ws::feed::sync_state::mint_socket_id;
use crate::sync_ws::feed::ui::ui_state_seed_frames;
use crate::sync_ws::feed::worker_frames::{worker_routable_frame, worker_routable_seed_frames};
use crate::sync_ws::frame_meta::{FeedLane, SyncFrameMeta};
use crate::sync_ws::resource_index::SyncResourceIndex;
use crate::sync_ws::v1_delivery::V1Delivery;
use crate::workers::registry::list_routable_fps;

/// A v1 socket's delivery at open, and what its seed leaves for the open to do.
#[derive(Debug)]
pub struct V1Open {
    /// The delivery, already pacing its seed on a `flow=1` socket.
    pub delivery: V1Delivery,
    /// The retained frames a socket without `flow=1` sends at once.
    pub unpaced: Vec<FeedFrame>,
    /// Fired when a paced seed is done; the backfill waits on it.
    pub seeded: Option<oneshot::Receiver<()>>,
}

/// Build a v1 socket's delivery around its retained seed
/// (`sync-feed.ts:76-84, 282-287`): paced by acknowledgements on a `flow=1`
/// socket, sent in one burst on any other.
pub fn open_v1_delivery(
    flow_control: bool,
    index: &SyncResourceIndex,
    services: &CoordServices,
    browser_ui: bool,
) -> V1Open {
    let frames = retained_seed_frames(index, services, browser_ui);
    tracing::info!(
        event = "sync-ws",
        action = "v1_seed",
        frames = frames.len(),
        paced = flow_control,
        "the v1 retained seed is built"
    );
    if !flow_control {
        return V1Open {
            delivery: V1Delivery::new(false),
            unpaced: frames,
            seeded: None,
        };
    }
    let retained = frames.into_iter().map(FeedFrame::into_frame).collect();
    let (delivery, seeded) = V1Delivery::with_paced_seed(retained);
    V1Open {
        delivery,
        unpaced: Vec::new(),
        seeded: Some(seeded),
    }
}

/// Every retained snapshot a v1 socket may observe, routability first because
/// it is the most volatile (`sync-feed-seed.ts:76-116`).
#[must_use]
pub fn retained_seed_frames(
    index: &SyncResourceIndex,
    services: &CoordServices,
    browser_ui: bool,
) -> Vec<FeedFrame> {
    let routable = WorkerRoutableSet {
        fps: list_routable_fps(&services.workers),
    };
    let mut frames = vec![worker_routable_frame(&routable, &index.worker_fps)];
    let sources = RetainedSessionState::read(services);
    frames.extend(
        sources
            .collect(index, None, false)
            .into_iter()
            .map(|(frame, _)| frame),
    );
    if browser_ui {
        frames.extend(ui_state_seed_frames(services.ui_state.states()));
    }
    frames
}

/// Replay one v2 domain's retained state into its queue, ahead of the live
/// frames it buffered before `domain_ready` (`sync-feed-seed.ts:119-202`).
///
/// `admitted` narrows the terminal seed to the sessions the client's snapshot
/// token covered; every other domain has no such fence.
pub fn seed_domain(
    link: &SyncLink,
    services: &CoordServices,
    domain: SyncDomain,
    admitted: Option<&BTreeSet<String>>,
) {
    match domain {
        SyncDomain::Workers => {
            let routable = list_routable_fps(&services.workers);
            let snapshot_id = seed_snapshot_id();
            link.deliver_with(|state| {
                let fps: Vec<WorkerFp> = routable
                    .into_iter()
                    .filter(|fp| state.index.worker_fps.contains(fp))
                    .collect();
                for frame in worker_routable_seed_frames(&fps, &snapshot_id) {
                    deliver_retained(state, frame, domain, None);
                }
                None
            });
        }
        SyncDomain::Terminal => {
            let sources = RetainedSessionState::read(services);
            link.deliver_with(|state| {
                for (frame, session_id) in sources.collect(&state.index, admitted, true) {
                    deliver_retained(state, frame, domain, Some(session_id));
                }
                None
            });
        }
        _ => return,
    }
    tracing::info!(event = "sync-ws", action = "domain_seeded", domain = ?domain, admitted = admitted.map(BTreeSet::len), "a Sync domain's retained state was replayed");
}

/// The session-keyed retained owners, read once for one seed.
struct RetainedSessionState {
    titles: Vec<SessionTitleUpdate>,
    signals: Vec<crate::events::bus_messages::SessionTerminalSignals>,
    activity: Vec<LastActivityUpdate>,
    statuses: Vec<AgentStatus>,
    viewers: BTreeMap<String, BTreeMap<String, TerminalGeometry>>,
}

impl RetainedSessionState {
    fn read(services: &CoordServices) -> Self {
        Self {
            titles: services.titles.title_snapshot(),
            signals: services.terminal_signals.snapshot(),
            activity: services.feed.last_activity().snapshot(),
            statuses: services.agents.status.snapshot(),
            viewers: services
                .views
                .viewer_projection()
                .into_iter()
                .map(|(session_id, viewers)| (session_id.as_str().to_owned(), viewers))
                .collect(),
        }
    }

    /// Titles, terminal signals, activity, agent status and (for a v2
    /// terminal seed) viewer
    /// rooms, each only for a session this socket observes and, when given,
    /// was admitted.
    fn collect(
        &self,
        index: &SyncResourceIndex,
        admitted: Option<&BTreeSet<String>>,
        viewers: bool,
    ) -> Vec<(FeedFrame, String)> {
        let seeds = |session_id: &str| {
            index.session_ids.contains(session_id)
                && admitted.is_none_or(|admitted| admitted.contains(session_id))
        };
        let mut frames = Vec::new();
        for title in self.titles.iter().filter(|title| seeds(&title.session_id)) {
            frames.push((session_title_frame(title), title.session_id.clone()));
        }
        for signals in self
            .signals
            .iter()
            .filter(|signals| seeds(&signals.session_id))
        {
            frames.push((
                crate::sync_ws::feed::signal_frames::session_terminal_signals_frame(signals),
                signals.session_id.clone(),
            ));
        }
        for update in self
            .activity
            .iter()
            .filter(|update| seeds(&update.session_id))
        {
            frames.push((last_activity_frame(update), update.session_id.clone()));
        }
        for status in &self.statuses {
            let session_id = status.common.session_id.as_str();
            if seeds(session_id) {
                let update = AgentStatusUpdate {
                    common: status.common.clone(),
                    active: status.active,
                };
                frames.push((agent_status_frame(&update), session_id.to_owned()));
            }
        }
        if viewers {
            let now = now_ms();
            for (session_id, room) in self.viewers.iter().filter(|(id, _)| seeds(id)) {
                frames.push((viewers_frame(session_id, room, now), session_id.clone()));
            }
        }
        frames
    }
}

/// One session's viewer room as the `viewers` presence a fresh socket renders
/// (`sync-feed-seed.ts:178-200`).
fn viewers_frame(
    session_id: &str,
    room: &BTreeMap<String, TerminalGeometry>,
    now_ms: u64,
) -> FeedFrame {
    let entries: Vec<_> = room
        .iter()
        .map(|(fp, geometry)| json!({"fp": fp, "viewerKey": fp, "cols": geometry.cols, "rows": geometry.rows, "lastMs": now_ms}))
        .collect();
    session_presence_frame(&SessionPresenceUpdate {
        session_id: session_id.to_owned(),
        data: json!({"kind": "viewers", "fps": room.keys().collect::<Vec<_>>(), "entries": entries}),
    })
}

/// Queue one retained frame ahead of the domain's buffered live segment.
fn deliver_retained(
    state: &mut LinkState,
    frame: FeedFrame,
    domain: SyncDomain,
    session_id: Option<String>,
) {
    let meta = SyncFrameMeta {
        domain: Some(domain),
        lane: FeedLane::Retained,
        session_id,
        before_buffered: true,
        ..SyncFrameMeta::default()
    };
    state.deliver(frame.with_meta(meta), now_ms());
}

/// The id one routable seed's chunks share, random as v2's `randomUUID()`.
fn seed_snapshot_id() -> String {
    mint_socket_id().unwrap_or_else(|error| {
        tracing::warn!(event = "sync-ws", action = "seed_snapshot_id_derived", error = %error, "no entropy for a seed snapshot id; deriving it from the clock");
        format!("seed-{}", now_ms())
    })
}
