//! The vocabulary the announced-channel barrier and its semantic retention
//! speak: their bounds, a channel's phases, why a channel drops and what the
//! drop cost, and the counters a log line reports.
//!
//! Read by `announced_barrier`, `announced_retention` and `announced_lane`.
//! Ports the exported types and constants of
//! `apps/coord/src/events/announced-channel-barrier.ts` and
//! `apps/coord/src/events/announced-channel-semantic-retention.ts`.

use std::time::Duration;

/// Frames one announced channel may hold before it drops (`:12`).
pub const ANNOUNCED_CHANNEL_MAX_FRAMES: usize = 64;

/// Bytes one announced channel may hold before it drops (`:13`).
pub const ANNOUNCED_CHANNEL_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// How long a channel may wait for its durable route before it drops (`:14`).
pub const ANNOUNCED_CHANNEL_MAX_WAIT: Duration = Duration::from_millis(3_000);

/// Channels with a retained semantic record, early and recovery together.
pub const SEMANTIC_METADATA_MAX_CHANNELS: usize = 64;

/// The largest encoded metadata frame the channel barrier retains: the 4 KiB a
/// title-and-activity record needs, plus the worker's 256 KiB OSC 52 cap
/// (`roost-term` `CLIPBOARD_WRITE_MAX_BYTES`), so a clipboard write that lands
/// before its channel is announced is held rather than dropped.
pub const SEMANTIC_METADATA_MAX_BYTES: u64 = 4 * 1024 + 256 * 1024;

/// How long a metadata fact may wait for its channel's announcement.
pub const SEMANTIC_METADATA_PREANNOUNCE_MAX: Duration = Duration::from_millis(3_000);

/// How long a fact parked after cell loss waits for its exact route.
pub const SEMANTIC_METADATA_RECOVERY_MAX: Duration = Duration::from_millis(30_000);

/// Where a channel is in its own lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelPhase {
    /// Announced, waiting for the durable append to commit.
    Pending,
    /// The route committed; buffered frames are draining in arrival order.
    Draining,
}

/// Why a channel's held frames were released without delivery (`:15-22`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DropReason {
    /// A bound was hit: frames, bytes, compactness, or the socket budget.
    #[error("overflow")]
    Overflow,
    /// The durable route did not commit inside [`ANNOUNCED_CHANNEL_MAX_WAIT`].
    #[error("timeout")]
    Timeout,
    /// A delta that does not continue the run, or a frame no lane holds.
    #[error("out_of_order")]
    OutOfOrder,
    /// The durable index bound a different session than the announcement.
    #[error("mapping_mismatch")]
    MappingMismatch,
    /// A new announcement for the same channel replaced this one.
    #[error("superseded")]
    Superseded,
    /// The durable append failed.
    #[error("append_failed")]
    AppendFailed,
    /// A held frame's delivery failed mid-drain.
    #[error("publish_failed")]
    PublishFailed,
}

impl DropReason {
    /// Whether this drop is a cell loss that parks the channel's latest
    /// metadata fact for recovery: a bound, a timeout or a gap while pending.
    #[must_use]
    pub fn parks_metadata(self) -> bool {
        matches!(self, Self::Overflow | Self::Timeout | Self::OutOfOrder)
    }
}

/// What a drop cost, reported so the screen replica of exactly that session
/// is invalidated (`worker-ws-upgrade.ts:21-28`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelDrop {
    /// The worker-local PTY id.
    pub channel_id: u32,
    /// The session the announcement named.
    pub session_id: String,
    /// Why the frames were released.
    pub reason: DropReason,
    /// Where the channel was when it dropped.
    pub phase: ChannelPhase,
    /// Cell frames lost, the refused one included.
    pub cell_frames: usize,
    /// Metadata frames lost or parked, the refused one included.
    pub metadata_frames: usize,
    /// Raw PTY frames lost, the refused one included.
    pub binary_frames: usize,
    /// Raw PTY bytes lost, the refused frame's included.
    pub binary_bytes: u64,
}

/// What `AnnouncedChannelBarrier::enqueue` did with a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// The channel is not announced, so the frame flows the ordinary way.
    NotAnnounced,
    /// The frame is held behind the durable append.
    Buffered,
    /// The frame was refused and the channel was dropped with it.
    Dropped,
}

/// Counters for the socket's overflow line, barrier and retention together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BarrierStats {
    /// Channels currently announced.
    pub channels: usize,
    /// Frames held, parked metadata included.
    pub frames: usize,
    /// Bytes held, parked metadata included.
    pub bytes: u64,
    /// Channels still waiting for their durable route.
    pub pending: usize,
    /// Channels draining.
    pub draining: usize,
    /// Metadata facts waiting for their channel's announcement.
    pub pre_announced_metadata: usize,
    /// Metadata facts parked after cell loss.
    pub recovery_metadata: usize,
}
