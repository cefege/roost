//! The announced-channel barrier: a channel's first terminal frames wait
//! behind the durable `opened`/`respawned` that makes the channel routable,
//! then publish in arrival order once the route commits.
//!
//! One per worker socket, held by `worker_link::announced_lane`. Ports
//! `apps/coord/src/events/announced-channel-barrier.ts`; the metadata a
//! cell-loss drop parks is `announced_retention`'s, and one channel's held
//! state is `announced_channel`'s. No socket and no database: the caller
//! supplies `now` wherever a wait starts, the budget, and the delivery.
//!
//! A cell sequence gap drops the channel, because a lost delta is not
//! recoverable by waiting; a `full` is exempt and restarts the run. Delivery is
//! a synchronous callback on the socket's one task, so v2's frames arriving
//! mid-drain cannot occur and the drain runs start to finish.

use std::collections::HashMap;

use roost_protocol::wire::coord_worker::{CoordWorkerUpstream, TerminalMetadata};
use tokio::time::Instant;

use crate::worker_link::announced_channel::{Channel, LaneCounts, release_frame, remove_lane};
use crate::worker_link::announced_retention::{RetainedMetadata, SemanticRetention};
use crate::worker_link::announced_types::{
    ANNOUNCED_CHANNEL_MAX_WAIT, BarrierStats, ChannelDrop, ChannelPhase, DropReason, EnqueueOutcome,
};
use crate::worker_link::retained_budget::RetainedWorkBudget;

/// Where every drop is reported: the screen replica's invalidation in
/// production (`worker-ws-upgrade.ts:21-28`), a recorder in tests.
pub type ChannelDropSink = Box<dyn FnMut(&ChannelDrop) + Send + Sync>;

/// One worker socket's announced-channel barrier.
pub struct AnnouncedChannelBarrier {
    channels: HashMap<u32, Channel>,
    retention: SemanticRetention,
    on_drop: ChannelDropSink,
}

impl std::fmt::Debug for AnnouncedChannelBarrier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnnouncedChannelBarrier")
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl AnnouncedChannelBarrier {
    /// A barrier reporting every drop to `on_drop`.
    #[must_use]
    pub fn new(on_drop: ChannelDropSink) -> Self {
        Self {
            channels: HashMap::new(),
            retention: SemanticRetention::default(),
            on_drop,
        }
    }

    /// `(channel_id, session_id)` has a durable append in flight. A repeat
    /// supersedes; a same-session recovery and an early fact move in first.
    pub fn announce(
        &mut self,
        channel_id: u32,
        session_id: &str,
        now: Instant,
        budget: &mut RetainedWorkBudget,
    ) {
        self.fail(channel_id, DropReason::Superseded, budget);
        let recovered = self
            .retention
            .take_recovery_for_session(channel_id, session_id, budget);
        let mut channel = Channel::new(session_id, now + ANNOUNCED_CHANNEL_MAX_WAIT);
        let early = self.retention.take_pre_announced(channel_id);
        for fact in recovered.into_iter().chain(early) {
            channel.append_retained_metadata(fact);
        }
        self.channels.insert(channel_id, channel);
    }

    /// Whether a channel's terminal frames are held at all.
    #[must_use]
    pub fn is_announced(&self, channel_id: u32) -> bool {
        self.channels.contains_key(&channel_id)
    }

    /// See `SemanticRetention::reconcile_mapped_route`.
    pub fn reconcile_retained_metadata(
        &mut self,
        channel_id: u32,
        session_id: &str,
        budget: &mut RetainedWorkBudget,
    ) -> bool {
        self.retention
            .reconcile_mapped_route(channel_id, session_id, budget)
    }

    /// See `SemanticRetention::retain_unannounced`.
    pub fn retain_unannounced_metadata(
        &mut self,
        channel_id: u32,
        metadata: &TerminalMetadata,
        encoded_bytes: u64,
        now: Instant,
        budget: &mut RetainedWorkBudget,
    ) -> bool {
        self.retention
            .retain_unannounced(channel_id, metadata, encoded_bytes, now, budget)
    }

    /// Hold one frame behind the channel's durable append; a refusal drops
    /// the whole channel, because a partial baseline is worse than none.
    pub fn enqueue(
        &mut self,
        channel_id: u32,
        frame: CoordWorkerUpstream,
        encoded_bytes: u64,
        now: Instant,
        budget: &mut RetainedWorkBudget,
    ) -> EnqueueOutcome {
        let Some(channel) = self.channels.get_mut(&channel_id) else {
            return EnqueueOutcome::NotAnnounced;
        };
        match channel.admit(frame, encoded_bytes, budget) {
            Ok(()) => EnqueueOutcome::Buffered,
            Err((reason, rejected)) => {
                if let Some(channel) = self.channels.remove(&channel_id) {
                    self.report_drop(channel_id, channel, reason, rejected, now, budget);
                }
                EnqueueOutcome::Dropped
            }
        }
    }

    /// The durable route committed: deliver the held frames in arrival order.
    /// `mapping_matches` is whether the durable index bound this exact
    /// session; a channel already gone defers to its parked recovery.
    /// `Ok(true)` only when everything held was delivered.
    pub fn commit<E>(
        &mut self,
        channel_id: u32,
        session_id: &str,
        mapping_matches: bool,
        budget: &mut RetainedWorkBudget,
        deliver: &mut dyn FnMut(CoordWorkerUpstream) -> Result<(), E>,
    ) -> Result<bool, E> {
        let Some(channel) = self.channels.get(&channel_id) else {
            return self.retention.commit_recovery(
                channel_id,
                session_id,
                mapping_matches,
                budget,
                deliver,
            );
        };
        if channel.session_id != session_id {
            return Ok(false);
        }
        let Some(mut channel) = self.channels.remove(&channel_id) else {
            return Ok(false);
        };
        let now = Instant::now();
        if !mapping_matches {
            let none = LaneCounts::default();
            self.report_drop(
                channel_id,
                channel,
                DropReason::MappingMismatch,
                none,
                now,
                budget,
            );
            return Ok(false);
        }
        channel.phase = ChannelPhase::Draining;
        channel.metadata = None;
        let mut held = std::mem::take(&mut channel.buffered).into_iter();
        while let Some(mut next) = held.next() {
            channel.bytes -= next.encoded_bytes;
            let lane = next.lane;
            release_frame(&mut next, budget);
            let delivered = deliver(next.frame);
            remove_lane(&mut channel.counts, lane);
            if let Err(failure) = delivered {
                channel.buffered = held.collect();
                let none = LaneCounts::default();
                self.report_drop(
                    channel_id,
                    channel,
                    DropReason::PublishFailed,
                    none,
                    now,
                    budget,
                );
                return Err(failure);
            }
        }
        Ok(true)
    }

    /// Drop a live announcement; an `AppendFailed` for a channel with none
    /// discards its parked recovery instead.
    pub fn fail(&mut self, channel_id: u32, reason: DropReason, budget: &mut RetainedWorkBudget) {
        match self.channels.remove(&channel_id) {
            Some(channel) => {
                let none = LaneCounts::default();
                self.report_drop(channel_id, channel, reason, none, Instant::now(), budget);
            }
            None if reason == DropReason::AppendFailed => {
                self.retention.discard_recovery(channel_id, budget);
            }
            None => {}
        }
    }

    /// Drop every channel whose wait ran out and forget every expired record.
    pub fn expire(&mut self, now: Instant, budget: &mut RetainedWorkBudget) {
        let expired: Vec<u32> = self
            .channels
            .iter()
            .filter(|(_, channel)| channel.deadline <= now)
            .map(|(channel_id, _)| *channel_id)
            .collect();
        for channel_id in expired {
            if let Some(channel) = self.channels.remove(&channel_id) {
                let none = LaneCounts::default();
                self.report_drop(channel_id, channel, DropReason::Timeout, none, now, budget);
            }
        }
        self.retention.expire(now, budget);
    }

    /// The earliest channel or record expiry, for the read loop's timer.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        let channels = self.channels.values().map(|channel| channel.deadline);
        channels.chain(self.retention.next_deadline()).min()
    }

    /// The counters, barrier and retention together.
    #[must_use]
    pub fn stats(&self) -> BarrierStats {
        let semantic = self.retention.stats();
        let mut stats = BarrierStats {
            channels: self.channels.len(),
            frames: semantic.frames,
            bytes: semantic.bytes,
            pre_announced_metadata: semantic.pre_announced,
            recovery_metadata: semantic.recovery,
            ..BarrierStats::default()
        };
        for channel in self.channels.values() {
            stats.frames += channel.buffered.len();
            stats.bytes += channel.bytes;
            match channel.phase {
                ChannelPhase::Pending => stats.pending += 1,
                ChannelPhase::Draining => stats.draining += 1,
            }
        }
        stats
    }

    /// Release a removed channel, park its latest fact when the drop is a
    /// pending cell loss, and report what it cost.
    fn report_drop(
        &mut self,
        channel_id: u32,
        channel: Channel,
        reason: DropReason,
        rejected: LaneCounts,
        now: Instant,
        budget: &mut RetainedWorkBudget,
    ) {
        let parked_index = (channel.phase == ChannelPhase::Pending && reason.parks_metadata())
            .then_some(channel.metadata)
            .flatten();
        let report = ChannelDrop {
            channel_id,
            session_id: channel.session_id,
            reason,
            phase: channel.phase,
            cell_frames: channel.counts.cell_frames + rejected.cell_frames,
            metadata_frames: channel.counts.metadata_frames + rejected.metadata_frames,
            binary_frames: channel.counts.binary_frames + rejected.binary_frames,
            binary_bytes: channel.counts.binary_bytes + rejected.binary_bytes,
        };
        let mut parked = None;
        for (index, mut frame) in channel.buffered.into_iter().enumerate() {
            match frame.frame {
                CoordWorkerUpstream::TerminalMetadata(metadata)
                    if Some(index) == parked_index && frame.retained =>
                {
                    let encoded_bytes = frame.encoded_bytes;
                    parked = Some(RetainedMetadata {
                        metadata,
                        encoded_bytes,
                    });
                }
                _ => release_frame(&mut frame, budget),
            }
        }
        if let Some(fact) = parked {
            self.retention
                .park_recovery(channel_id, &report.session_id, fact, now, budget);
        }
        (self.on_drop)(&report);
    }
}
