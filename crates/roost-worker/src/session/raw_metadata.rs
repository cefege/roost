//! The old-coordinator raw-metadata compatibility lane: v2
//! `apps/worker/src/session/session-raw-metadata.ts`. Copied PTY bytes are staged
//! only while semantic metadata is NOT negotiated, bounded per channel and in
//! aggregate, and dispatched round-robin (32 frames / 4 ms per turn, a 16 ms
//! trailing window). `session::emit` stages; the cadence dispatches into the link.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use roost_protocol::wire::brand::ChannelId;

/// v2 `RAW_METADATA_CHANNEL_CAP_BYTES`.
pub const RAW_METADATA_CHANNEL_CAP_BYTES: usize = 256 * 1024;
/// v2 `RAW_METADATA_AGGREGATE_CAP_BYTES`.
pub const RAW_METADATA_AGGREGATE_CAP_BYTES: usize = 2 * 1024 * 1024;
/// v2 `RAW_METADATA_DISPATCH_FRAME_BUDGET`.
pub const RAW_METADATA_DISPATCH_FRAME_BUDGET: usize = 32;
/// v2 `RAW_METADATA_DISPATCH_MAX_TURN_MS`.
pub const RAW_METADATA_DISPATCH_MAX_TURN: Duration = Duration::from_millis(4);
/// v2's trailing window: `CELL_EMIT_COALESCE_MS`.
pub const RAW_METADATA_TRAILING_WINDOW: Duration = Duration::from_millis(16);

/// One chunk of raw PTY bytes and the logical offset of its END.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedRawFrame {
    pub channel_id: ChannelId,
    pub end_seq: u64,
    pub bytes: Vec<u8>,
}

/// What the link did with one raw frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawSend {
    Accepted,
    /// The link refused it: the channel's whole queue is dropped (v2 `dropRawMetadataQueue`).
    Dropped,
}

/// v2 `rawMetadataWake`: an immediate dispatch, or the trailing timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawWake {
    Idle,
    Immediate,
    At(Instant),
}

/// The staging area for the compatibility lane, and the one place its caps live.
#[derive(Debug)]
pub struct RawMetadataStage {
    negotiated: bool,
    queues: HashMap<ChannelId, (VecDeque<StagedRawFrame>, usize)>,
    ready: VecDeque<ChannelId>,
    ready_set: HashSet<ChannelId>,
    queued_bytes: usize,
    wake: RawWake,
    /// Frames refused because a cap was reached, for the diagnostic snapshot.
    dropped: u64,
}

impl Default for RawMetadataStage {
    fn default() -> Self {
        Self {
            negotiated: false,
            queues: HashMap::new(),
            ready: VecDeque::new(),
            ready_set: HashSet::new(),
            queued_bytes: 0,
            wake: RawWake::Idle,
            dropped: 0,
        }
    }
}

impl RawMetadataStage {
    pub fn semantic_metadata_negotiated(&self) -> bool {
        self.negotiated
    }

    /// Record the coordinator's answer; negotiation empties the lane.
    pub fn set_semantic_metadata_negotiated(&mut self, negotiated: bool) {
        if self.negotiated == negotiated {
            return;
        }
        self.negotiated = negotiated;
        if negotiated {
            let channels: Vec<ChannelId> = self.queues.keys().copied().collect();
            for channel_id in channels {
                self.forget_channel(channel_id);
            }
        }
        tracing::info!(
            negotiated,
            "the terminal-metadata negotiation changed the raw lane"
        );
    }

    /// v2 `_enqueueRawMetadata`: stage one copied chunk unless a cap says no.
    /// Returns whether an immediate dispatch became owed.
    pub fn stage(&mut self, channel_id: ChannelId, end_seq: u64, chunk: &[u8]) -> bool {
        if self.negotiated || chunk.is_empty() {
            return false;
        }
        let channel_bytes = self.queues.get(&channel_id).map_or(0, |(_, bytes)| *bytes);
        if chunk.len() > RAW_METADATA_CHANNEL_CAP_BYTES
            || channel_bytes + chunk.len() > RAW_METADATA_CHANNEL_CAP_BYTES
            || self.queued_bytes + chunk.len() > RAW_METADATA_AGGREGATE_CAP_BYTES
        {
            self.dropped = self.dropped.saturating_add(1);
            tracing::warn!(
                %channel_id,
                frame_bytes = chunk.len(),
                channel_bytes,
                aggregate_bytes = self.queued_bytes,
                "transport.raw_metadata_drop: the staging caps were reached"
            );
            return false;
        }
        let (queue, bytes) = self.queues.entry(channel_id).or_default();
        queue.push_back(StagedRawFrame {
            channel_id,
            end_seq,
            bytes: chunk.to_vec(),
        });
        *bytes += chunk.len();
        self.queued_bytes += chunk.len();
        self.mark_ready(channel_id);
        if self.wake == RawWake::Idle {
            self.wake = RawWake::Immediate;
            return true;
        }
        false
    }

    /// Whether a dispatch is owed at `now`.
    pub fn dispatch_due(&self, now: Instant) -> bool {
        match self.wake {
            RawWake::Idle => false,
            RawWake::Immediate => true,
            RawWake::At(due) => due <= now,
        }
    }

    /// The trailing timer, while one is armed.
    pub fn next_wake(&self) -> Option<Instant> {
        match self.wake {
            RawWake::At(due) => Some(due),
            _ => None,
        }
    }

    /// v2 `drainRawMetadata`: one bounded, round-robin turn. `live` is the
    /// session table's answer; `send` puts one frame on the link.
    pub fn dispatch(
        &mut self,
        live: &dyn Fn(ChannelId) -> bool,
        send: &mut dyn FnMut(&StagedRawFrame) -> RawSend,
        now: Instant,
    ) -> usize {
        self.wake = RawWake::Idle;
        // The turn bound is real elapsed work, independent of the cadence clock.
        let turn_started = Instant::now();
        let mut frames = 0usize;
        while frames < RAW_METADATA_DISPATCH_FRAME_BUDGET
            && turn_started.elapsed() < RAW_METADATA_DISPATCH_MAX_TURN
        {
            let Some(channel_id) = self.ready.pop_front() else {
                break;
            };
            self.ready_set.remove(&channel_id);
            if self.negotiated || !live(channel_id) {
                self.forget_channel(channel_id);
                continue;
            }
            let Some(frame) = self
                .queues
                .get(&channel_id)
                .and_then(|(queue, _)| queue.front().cloned())
            else {
                self.queues.remove(&channel_id);
                continue;
            };
            frames += 1;
            if send(&frame) == RawSend::Dropped {
                self.drop_queue(channel_id);
                continue;
            }
            self.release_head(channel_id);
            tracing::debug!(%channel_id, len = frame.bytes.len(), end_seq = frame.end_seq, "emit_upstream");
        }
        // Keep one trailing window after a real drain so a following burst joins
        // the metadata cadence instead of racing a cell coalesce.
        if frames > 0 || !self.ready.is_empty() {
            self.wake = RawWake::At(now + RAW_METADATA_TRAILING_WINDOW);
        }
        frames
    }

    pub fn staged_bytes(&self) -> usize {
        self.queued_bytes
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped
    }

    /// One channel's staged frames and bytes (v2 `rawMetadataQueues.get(ch)`).
    pub fn channel_backlog(&self, channel_id: ChannelId) -> (usize, usize) {
        self.queues
            .get(&channel_id)
            .map_or((0, 0), |(queue, bytes)| (queue.len(), *bytes))
    }

    /// v2 `disposeRawMetadataState`: one channel, without another's wake.
    pub fn forget_channel(&mut self, channel_id: ChannelId) {
        if self.ready_set.remove(&channel_id) {
            self.ready.retain(|queued| *queued != channel_id);
        }
        if let Some((_, bytes)) = self.queues.remove(&channel_id) {
            self.queued_bytes = self.queued_bytes.saturating_sub(bytes);
        }
        if self.ready.is_empty() && matches!(self.wake, RawWake::At(_)) {
            self.wake = RawWake::Idle;
        }
    }

    fn mark_ready(&mut self, channel_id: ChannelId) {
        if self.ready_set.insert(channel_id) {
            self.ready.push_back(channel_id);
        }
    }

    fn release_head(&mut self, channel_id: ChannelId) {
        let Some((queue, bytes)) = self.queues.get_mut(&channel_id) else {
            return;
        };
        let Some(frame) = queue.pop_front() else {
            return;
        };
        *bytes = bytes.saturating_sub(frame.bytes.len());
        self.queued_bytes = self.queued_bytes.saturating_sub(frame.bytes.len());
        if queue.is_empty() {
            self.queues.remove(&channel_id);
        } else {
            self.mark_ready(channel_id);
        }
    }

    fn drop_queue(&mut self, channel_id: ChannelId) {
        let Some((queue, bytes)) = self.queues.remove(&channel_id) else {
            return;
        };
        self.queued_bytes = self.queued_bytes.saturating_sub(bytes);
        if self.ready_set.remove(&channel_id) {
            self.ready.retain(|queued| *queued != channel_id);
        }
        tracing::warn!(%channel_id, frames = queue.len(), bytes, "transport.raw_metadata_drop: the link refused a raw frame");
    }
}
