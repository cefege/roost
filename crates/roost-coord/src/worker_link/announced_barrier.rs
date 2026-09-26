//! The announced-channel barrier itself: announce, enqueue, commit, fail.
//!
//! Owned by the coordinator's worker link, one per worker connection. The
//! vocabulary it speaks is in `announced_types`; this file is only the machine.
//! It holds no socket, no database and no clock -- the caller supplies `now_ms`
//! and performs delivery -- which is what lets the whole ordering contract be
//! tested without a network.
//!
//! THE ORDERING RULE, IN ONE PARAGRAPH. A worker sends `opened` and its first
//! cell frames back to back, and the cell frames overtake the durable append. So
//! a cell frame can reach the coordinator before the `(worker, channel) ->
//! session` route that makes it addressable. The symptom is a terminal that
//! never paints -- not an ordering error -- which is why the fix lives here
//! rather than in whatever notices the symptom. `announce` opens a channel, the
//! cell/binary/metadata frames for it are held, and `commit` -- called on the
//! same socket lane after the durable append settles -- drains them in arrival
//! order. A channel that is not committed inside [`MAX_WAIT_MS`], or that
//! overflows, is dropped and its session's terminal stream is invalidated.
//!
//! WHY A CELL SEQUENCE GAP DROPS THE WHOLE CHANNEL. A delta whose `seq` is not
//! the exact successor of the last held one means a frame was lost, and a lost
//! frame is not recoverable by waiting: the recipient's baseline is already
//! wrong. This is the only place on the coordinator that validates cell
//! ordering before a frame reaches a replica, which is why it knows about
//! `full` and `seq` at all. A `full` frame is exempt from the rule and *resets*
//! the run, because a full after a gap is precisely the repair that makes the
//! channel usable again -- refusing it would leave the recipient permanently
//! stuck rather than momentarily wrong.

use std::collections::HashMap;

use super::announced_types::{
    BarrierStats, ChannelDrop, ChannelPhase, CommitOutcome, DropReason, EnqueueOutcome, FrameLane,
    MAX_BYTES, MAX_FRAMES, MAX_WAIT_MS, RetainOutcome, RetainedFrame, RetainedWorkBudget,
};

/// One announced channel's held state.
#[derive(Debug, Clone)]
struct Channel {
    session_id: String,
    phase: ChannelPhase,
    buffered: Vec<RetainedFrame>,
    bytes: u64,
    cell_frames: usize,
    metadata_frames: usize,
    binary_frames: usize,
    binary_bytes: u64,
    saw_cell_frame: bool,
    last_cell_seq: u64,
    announced_at_ms: u64,
}

impl Channel {
    fn new(session_id: String, announced_at_ms: u64) -> Self {
        Self {
            session_id,
            phase: ChannelPhase::Pending,
            buffered: Vec::new(),
            bytes: 0,
            cell_frames: 0,
            metadata_frames: 0,
            binary_frames: 0,
            binary_bytes: 0,
            saw_cell_frame: false,
            last_cell_seq: 0,
            announced_at_ms,
        }
    }

    fn report(&self, channel_id: u32, reason: DropReason) -> ChannelDrop {
        ChannelDrop {
            channel_id,
            session_id: self.session_id.clone(),
            reason,
            phase: self.phase,
            cell_frames: self.cell_frames,
            metadata_frames: self.metadata_frames,
            binary_frames: self.binary_frames,
            binary_bytes: self.binary_bytes,
        }
    }
}

/// One worker connection's announced-channel barrier.
///
/// The drop callback is a **parameter on each call** rather than a stored
/// closure. v2 constructs the barrier with its `onDrop` wired at construction
/// (`apps/coord/src/workers/worker-ws-upgrade.ts:21-28`), but the reason that
/// matters is the same either way: the callback reaches the terminal view hub,
/// and the hub does not exist when the upgrade handler builds this. Passing it
/// per call keeps the barrier free of a lifetime parameter and free of a
/// half-initialised hub, and it is what lets a test collect the drops without
/// standing up a coordinator.
#[derive(Debug)]
pub struct AnnouncedBarrier {
    channels: HashMap<u32, Channel>,
    budget: RetainedWorkBudget,
}

impl AnnouncedBarrier {
    /// A barrier charging the given socket-wide budget.
    ///
    /// The budget is passed in rather than created here because it is the
    /// **socket's**, shared with the ordered frame queue
    /// (`apps/coord/src/workers/worker-ws-upgrade.ts:148`): one worker opening
    /// sixty-four channels must exhaust one budget, not sixty-four of them.
    #[must_use]
    pub fn new(budget: RetainedWorkBudget) -> Self {
        Self {
            channels: HashMap::new(),
            budget,
        }
    }

    /// The current counters, for the `1009` decision and for diagnostics.
    #[must_use]
    pub fn stats(&self) -> BarrierStats {
        let mut stats = BarrierStats {
            channels: self.channels.len(),
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

    /// Whether a channel is announced, so the socket layer knows to hold its
    /// frames at all.
    #[must_use]
    pub fn is_announced(&self, channel_id: u32) -> bool {
        self.channels.contains_key(&channel_id)
    }

    /// Declare that `(channel_id, session_id)` now has a durable append in
    /// flight.
    ///
    /// A repeat announcement **supersedes** the previous one rather than
    /// merging with it: the old channel's frames were held for a route that is
    /// being replaced, and delivering them would bind cells to a session that no
    /// longer owns the channel.
    pub fn announce(
        &mut self,
        channel_id: u32,
        session_id: &str,
        now_ms: u64,
        on_drop: &mut dyn FnMut(ChannelDrop),
    ) {
        if self.channels.contains_key(&channel_id) {
            self.drop_channel(channel_id, DropReason::Superseded, on_drop);
        }
        self.channels
            .insert(channel_id, Channel::new(session_id.to_string(), now_ms));
    }

    /// Hold one frame behind the channel's durable append.
    ///
    /// The refusal rules, in order, each of which drops the whole channel because
    /// a partially-delivered baseline is worse than none:
    ///
    /// 1. not announced: the frame flows the ordinary way and nothing is held;
    /// 2. a zero encoded size: `overflow`;
    /// 3. [`MAX_FRAMES`] already held, or [`MAX_BYTES`] already held: `overflow`;
    /// 4. a delta that does not continue the run exactly: `out_of_order`;
    /// 5. the socket-wide budget refusing the charge: `overflow`.
    pub fn enqueue(
        &mut self,
        channel_id: u32,
        frame: RetainedFrame,
        on_drop: &mut dyn FnMut(ChannelDrop),
    ) -> EnqueueOutcome {
        let Some(channel) = self.channels.get(&channel_id) else {
            return EnqueueOutcome::NotAnnounced;
        };

        if frame.encoded_bytes == 0
            || channel.buffered.len() >= MAX_FRAMES
            || channel.bytes.saturating_add(frame.encoded_bytes) > MAX_BYTES as u64
        {
            return self.drop_and(channel_id, DropReason::Overflow, on_drop);
        }

        if let FrameLane::Cell { full: false, seq } = frame.lane
            && (!channel.saw_cell_frame || seq != channel.last_cell_seq.saturating_add(1))
        {
            return self.drop_and(channel_id, DropReason::OutOfOrder, on_drop);
        }

        if self.budget.retain(frame.encoded_bytes) != RetainOutcome::Retained {
            return self.drop_and(channel_id, DropReason::Overflow, on_drop);
        }

        if let Some(channel) = self.channels.get_mut(&channel_id) {
            match frame.lane {
                FrameLane::Cell { full: false, seq } => {
                    channel.saw_cell_frame = true;
                    channel.last_cell_seq = seq;
                }
                FrameLane::Cell { full: true, seq } => {
                    // A full establishes a new baseline, so the run restarts
                    // here and the next delta must be this frame's successor.
                    channel.saw_cell_frame = true;
                    channel.last_cell_seq = seq;
                }
                FrameLane::Metadata => channel.metadata_frames += 1,
                FrameLane::Binary { bytes } => {
                    channel.binary_frames += 1;
                    channel.binary_bytes += bytes;
                }
            }
            channel.cell_frames += u32::from(matches!(frame.lane, FrameLane::Cell { .. })) as usize;
            channel.buffered.push(frame);
            channel.bytes = channel.bytes.saturating_add(frame.encoded_bytes);
        }
        EnqueueOutcome::Buffered
    }

    /// The durable route committed. Release the held frames in arrival order.
    ///
    /// `mapping_matches` is the caller's check that the durable index really
    /// bound `(worker, channel)` to the announced session. It is a value rather
    /// than a closure because the caller has already read it: a barrier that
    /// asked later could act on a route that changed in between, and a
    /// `mapping_mismatch` is a drop -- the one refusal that means the durable
    /// state and the announcement disagree, which is exactly the case where a
    /// stale re-read would deliver cells to the wrong session.
    pub fn commit(
        &mut self,
        channel_id: u32,
        session_id: &str,
        mapping_matches: bool,
        on_drop: &mut dyn FnMut(ChannelDrop),
    ) -> CommitOutcome {
        let Some(channel) = self.channels.get(&channel_id) else {
            return CommitOutcome::NotAnnounced;
        };
        if channel.session_id != session_id {
            return CommitOutcome::SessionMismatch;
        }
        if !mapping_matches {
            self.drop_channel(channel_id, DropReason::MappingMismatch, on_drop);
            return CommitOutcome::MappingMismatch;
        }
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            channel.phase = ChannelPhase::Draining;
        }
        let frames = self
            .channels
            .get(&channel_id)
            .map_or(0, |channel| channel.buffered.len());
        self.release_channel(channel_id);
        CommitOutcome::Drained { frames }
    }

    /// The durable append failed, or a delivery threw. Drop the channel.
    ///
    /// Called from the socket queue's error handler
    /// (`apps/coord/src/workers/worker-ws-handler.ts:109-113`): a throw there
    /// is fatal, the socket is torn down, and the worker reconnects and replays
    /// whatever it never had acknowledged.
    pub fn fail(
        &mut self,
        channel_id: u32,
        reason: DropReason,
        on_drop: &mut dyn FnMut(ChannelDrop),
    ) {
        self.drop_channel(channel_id, reason, on_drop);
    }

    /// Drop every channel and release the whole budget. Called on socket close.
    pub fn clear(&mut self) {
        self.channels.clear();
    }

    /// Channels whose wait has run out, as `(channel_id, session_id)` pairs.
    ///
    /// The caller drives the timer, because this file takes no clock: a test
    /// advances an injected clock and asserts the pair comes back, with nothing
    /// sleeping and nothing flaky.
    #[must_use]
    pub fn expired(&self, now_ms: u64) -> Vec<(u32, String)> {
        self.channels
            .iter()
            .filter(|(_, channel)| {
                channel.phase == ChannelPhase::Pending
                    && now_ms.saturating_sub(channel.announced_at_ms) >= MAX_WAIT_MS
            })
            .map(|(channel_id, channel)| (*channel_id, channel.session_id.clone()))
            .collect()
    }

    fn drop_and(
        &mut self,
        channel_id: u32,
        reason: DropReason,
        on_drop: &mut dyn FnMut(ChannelDrop),
    ) -> EnqueueOutcome {
        self.drop_channel(channel_id, reason, on_drop);
        EnqueueOutcome::Dropped
    }

    fn drop_channel(
        &mut self,
        channel_id: u32,
        reason: DropReason,
        on_drop: &mut dyn FnMut(ChannelDrop),
    ) {
        let Some(channel) = self.channels.remove(&channel_id) else {
            return;
        };
        for frame in &channel.buffered {
            self.budget.release(frame.encoded_bytes);
        }
        on_drop(channel.report(channel_id, reason));
    }

    fn release_channel(&mut self, channel_id: u32) {
        let Some(channel) = self.channels.remove(&channel_id) else {
            return;
        };
        for frame in &channel.buffered {
            self.budget.release(frame.encoded_bytes);
        }
    }
}
