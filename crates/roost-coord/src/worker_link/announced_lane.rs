//! The announced-channel barrier's socket half: which durable frame announces
//! a channel, which terminal frame is held or retained instead of published
//! now, the commit that follows a durable append, and the drop that
//! invalidates the session's screen replica.
//!
//! Held by `worker_link::result_lane` beside the backlog whose budget it
//! charges; `link_session` reaches it through the lane. Ports
//! `createAnnouncedChannelBarrier` of `apps/coord/src/workers/worker-ws-upgrade.ts:21-28`
//! and the barrier paths of `apps/coord/src/workers/worker-ws-handler.ts`:
//! the queue's commit and failure (`:97-114`), `message`'s announce and
//! terminal fast path (`:281-338`).

use std::sync::Arc;

use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use roost_protocol::wire::{ChannelId, SessionEvent, SessionId, WorkerFp};
use tokio::time::Instant;

use crate::terminal_screen::replica::ScreenHub;
use crate::worker_link::announced_barrier::{AnnouncedChannelBarrier, ChannelDropSink};
use crate::worker_link::announced_types::{BarrierStats, ChannelDrop, DropReason};
use crate::worker_link::conn_types::SocketClose;
use crate::worker_link::dispatch::{DispatchOutcome, FrameClass, FrameDispatch, InboundFrame};
use crate::worker_link::frame_dispatch::WorkerFrameDispatcher;
use crate::worker_link::retained_budget::RetainedWorkBudget;

/// A durable frame's claim on a channel whose route its append will bind:
/// an `opened` this worker owns, or a `respawned` (which names no worker; the
/// commit refuses unless the durable index bound this socket's channel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Announcement {
    channel_id: u32,
    session_id: SessionId,
}

impl Announcement {
    /// The announcement a durable frame makes, if any.
    pub(super) fn of(frame: &InboundFrame, worker_fp: &WorkerFp) -> Option<Self> {
        let CoordWorkerUpstream::Event { event, .. } = &frame.frame else {
            return None;
        };
        let (session_id, channel) = match event {
            SessionEvent::Opened {
                session_id,
                worker_fp: claimed,
                channel,
                ..
            } if claimed == worker_fp => (session_id, channel),
            SessionEvent::Respawned {
                session_id,
                new_channel,
                ..
            } => (session_id, new_channel),
            _ => return None,
        };
        Some(Self {
            channel_id: channel.as_u32(),
            session_id: session_id.clone(),
        })
    }
}

/// Whether a frame takes v2's terminal fast path rather than the ordered lane.
pub(super) fn is_terminal_frame(frame: &CoordWorkerUpstream) -> bool {
    matches!(
        frame,
        CoordWorkerUpstream::CellGrid(_)
            | CoordWorkerUpstream::CellGridChunk(_)
            | CoordWorkerUpstream::Binary(_)
            | CoordWorkerUpstream::TerminalMetadata(_)
    )
}

/// Every drop invalidates exactly its session's screen replica, so the
/// replica requests a fresh baseline rather than folding over the hole.
pub(super) fn invalidate_screen_on_drop(
    screens: Arc<ScreenHub>,
    worker_fp: WorkerFp,
) -> ChannelDropSink {
    Box::new(move |drop: &ChannelDrop| {
        tracing::info!(%worker_fp, channel_id = drop.channel_id, session_id = %drop.session_id,
            reason = %drop.reason, phase = ?drop.phase, cell_frames = drop.cell_frames,
            metadata_frames = drop.metadata_frames, binary_frames = drop.binary_frames,
            binary_bytes = drop.binary_bytes, "worker link: announced channel dropped");
        let Ok(session_id) = SessionId::try_from(drop.session_id.as_str()) else {
            tracing::warn!(%worker_fp, session_id = %drop.session_id,
                "worker link: a dropped announcement names no addressable session");
            return;
        };
        let short_fp = worker_fp.as_str().get(..12).unwrap_or(worker_fp.as_str());
        let reason = format!("announced channel barrier {} on {short_fp}", drop.reason);
        screens.invalidate(&session_id, &reason);
    })
}

/// One socket's barrier, driven from the socket's frames.
#[derive(Debug)]
pub(super) struct AnnouncedLane {
    barrier: AnnouncedChannelBarrier,
}

impl AnnouncedLane {
    /// A barrier whose drops invalidate the dispatcher's screen replicas.
    pub(super) fn new(dispatcher: &WorkerFrameDispatcher) -> Self {
        let screens = Arc::clone(dispatcher.core.services.byte_hub.screens());
        let sink = invalidate_screen_on_drop(screens, dispatcher.handle.worker_fp.clone());
        Self {
            barrier: AnnouncedChannelBarrier::new(sink),
        }
    }

    /// Hold the announced channel's terminal frames from now on.
    pub(super) fn announce(
        &mut self,
        announcement: &Announcement,
        budget: &mut RetainedWorkBudget,
    ) {
        tracing::debug!(channel_id = announcement.channel_id,
            session_id = %announcement.session_id, "worker link: channel announced");
        let session_id = announcement.session_id.as_str();
        let now = Instant::now();
        self.barrier
            .announce(announcement.channel_id, session_id, now, budget);
    }

    /// v2's terminal fast path: hold a frame for an announced channel, retain
    /// a metadata fact whose route is not bound yet (or sits behind its own
    /// session's recovery), and hand everything else back to publish now.
    pub(super) fn route_terminal(
        &mut self,
        inbound: InboundFrame,
        encoded_bytes: u64,
        dispatcher: &WorkerFrameDispatcher,
        budget: &mut RetainedWorkBudget,
    ) -> Option<InboundFrame> {
        let now = Instant::now();
        // A wait that ran out while an append was in flight ends first, as
        // v2's timer would have ended it before this frame arrived.
        self.barrier.expire(now, budget);
        let channel_id = inbound.channel;
        if self.barrier.is_announced(channel_id) {
            self.barrier
                .enqueue(channel_id, inbound.frame, encoded_bytes, now, budget);
            return None;
        }
        if let CoordWorkerUpstream::TerminalMetadata(metadata) = &inbound.frame {
            let worker_fp = &dispatcher.handle.worker_fp;
            let mapped = dispatcher
                .core
                .services
                .byte_hub
                .resolve(worker_fp, metadata.channel_id);
            let retainable = match &mapped {
                None => true,
                Some(session_id) => self.barrier.reconcile_retained_metadata(
                    channel_id,
                    session_id.as_str(),
                    budget,
                ),
            };
            if retainable
                && self.barrier.retain_unannounced_metadata(
                    channel_id,
                    metadata,
                    encoded_bytes,
                    now,
                    budget,
                )
            {
                return None;
            }
        }
        Some(inbound)
    }

    /// After a durable append settled: drain the announced channel when the
    /// durable index bound it to this session, or drop it when the append
    /// failed. A dedupe mismatch still commits, as v2's `requestClose` does.
    pub(super) fn commit_after_append(
        &mut self,
        announcement: Option<Announcement>,
        appended: DispatchOutcome,
        dispatcher: &WorkerFrameDispatcher,
        budget: &mut RetainedWorkBudget,
    ) -> DispatchOutcome {
        let Some(announcement) = announcement else {
            return appended;
        };
        let Announcement {
            channel_id,
            session_id,
        } = announcement;
        self.barrier.expire(Instant::now(), budget);
        if let DispatchOutcome::Close(close) = appended
            && close != SocketClose::DedupeMismatch
        {
            self.barrier
                .fail(channel_id, DropReason::AppendFailed, budget);
            return appended;
        }
        let worker_fp = &dispatcher.handle.worker_fp;
        let byte_hub = &dispatcher.core.services.byte_hub;
        let mapping_matches = ChannelId::try_from(i64::from(channel_id))
            .ok()
            .and_then(|channel| byte_hub.resolve(worker_fp, channel))
            .is_some_and(|bound| bound == session_id);
        let mut deliver = |frame: CoordWorkerUpstream| {
            let live = InboundFrame {
                class: FrameClass::Live,
                channel: channel_id,
                frame,
            };
            match dispatcher.handle_now(worker_fp.as_str(), live) {
                DispatchOutcome::Close(close) => Err(close),
                DispatchOutcome::Handled | DispatchOutcome::Refused => Ok(()),
            }
        };
        let committed = self.barrier.commit(
            channel_id,
            session_id.as_str(),
            mapping_matches,
            budget,
            &mut deliver,
        );
        match committed {
            Ok(delivered) => {
                tracing::debug!(%worker_fp, channel_id, %session_id, mapping_matches, delivered,
                    "worker link: announced channel committed");
                appended
            }
            Err(close) => {
                self.barrier
                    .fail(channel_id, DropReason::AppendFailed, budget);
                DispatchOutcome::Close(close)
            }
        }
    }

    /// End every wait that ran out.
    pub(super) fn expire(&mut self, now: Instant, budget: &mut RetainedWorkBudget) {
        self.barrier.expire(now, budget);
    }

    /// The next wait to end, for the read loop's timer.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.barrier.next_deadline()
    }

    /// What the barrier holds, for the overflow line.
    pub(super) fn stats(&self) -> BarrierStats {
        self.barrier.stats()
    }
}
