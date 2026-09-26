//! The vocabulary the announced-channel barrier speaks: its bounds, its phases,
//! what a drop costs, and the socket-wide retention budget it charges against.
//!
//! Owned by the coordinator's worker link, one instance per worker connection
//! (`apps/coord/src/workers/worker-ws-upgrade.ts:110`) with an explicit
//! `on_drop` callback wired at construction (`worker-ws-upgrade.ts:21-28`) --
//! not a crate-root singleton, because the v2 unit tests construct one directly
//! and there is no global hub available at construction time.
//!
//! THE PROBLEM, IN ONE SENTENCE. A worker sends `opened` and its first cell
//! frames back to back, and the cell frames overtake the durable append, so a
//! cell frame can reach the coordinator before the `(worker, channel) ->
//! session` route that makes it addressable. The symptom is a terminal that
//! never paints, not an ordering error, which is why the fix has to be here
//! rather than in whatever notices the symptom.
//!
//! TWO NESTED MACHINES. Per channel: `Pending -> Draining -> gone`. Per link:
//! `hello -> replay -> snapshot -> live` (the worker's own machine, in
//! `protocol/spec/worker-link.md`). This file is the coordinator-side mirror of
//! "the durable `opened` must commit before this channel's bytes are routable",
//! and it holds only the short pre-publication interval.
//!
//! A NON-FULL CELL FRAME IS A SEQUENCE CLAIM. A delta whose `seq` is not the
//! exact successor of the last one is a gap, and a gap is not recoverable by
//! waiting: rule 5 below refuses the whole channel. That is why this file knows
//! about `full` and `seq` at all -- it is the only place on the coordinator that
//! validates cell ordering before the frame reaches a replica.

/// Frames one announced channel may hold before it is dropped.
pub const MAX_FRAMES: usize = 64;

/// Bytes one announced channel may hold before it is dropped.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// How long a channel may wait for its durable route before it is dropped.
pub const MAX_WAIT_MS: u64 = 3_000;

/// Where a channel is in its own lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelPhase {
    /// Announced, waiting for the durable append to commit.
    Pending,
    /// The route committed; buffered frames are draining in arrival order.
    Draining,
}

/// Why a channel's retained frames were released without delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DropReason {
    /// A bound was hit: frames, bytes, or the socket-wide work budget.
    #[error("overflow")]
    Overflow,
    /// The durable route did not commit inside [`MAX_WAIT_MS`].
    #[error("timeout")]
    Timeout,
    /// A delta arrived with a sequence gap, or a frame arrived that cannot be
    /// buffered at all.
    #[error("out_of_order")]
    OutOfOrder,
    /// The durable index bound a different session than the announcement named.
    #[error("mapping_mismatch")]
    MappingMismatch,
    /// A new announcement for the same channel replaced this one.
    #[error("superseded")]
    Superseded,
    /// The durable append failed.
    #[error("append_failed")]
    AppendFailed,
    /// A frame's delivery threw.
    #[error("publish_failed")]
    PublishFailed,
}

/// What a drop cost, reported so the terminal view hub can invalidate exactly
/// the stream that lost its baseline.
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
    /// How many cell frames were held.
    pub cell_frames: usize,
    /// How many metadata frames were held.
    pub metadata_frames: usize,
    /// How many raw PTY frames were held.
    pub binary_frames: usize,
    /// How many raw PTY bytes were held.
    pub binary_bytes: u64,
}

/// The shape of a buffered frame, without its payload.
///
/// The I/O layer owns the encoded bytes; this machine only needs to know which
/// lane a frame is on and what sequence it claims. Keeping the payload out is
/// what makes the whole barrier testable with three integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameLane {
    /// An authoritative full or delta. `full` frames establish a baseline and
    /// carry no sequence obligation; a delta must continue the run exactly.
    Cell { full: bool, seq: u64 },
    /// A compact terminal-metadata record. Coalescable: only the latest survives.
    Metadata,
    /// Raw PTY bytes. Never coalesced and never retained past a cell-loss drop.
    Binary { bytes: u64 },
}

/// A frame held behind an announcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetainedFrame {
    /// Which lane the frame is on.
    pub lane: FrameLane,
    /// The frame's encoded size on the wire.
    pub encoded_bytes: u64,
}

/// What [`AnnouncedBarrier::enqueue`] did with a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// The channel is not announced, so the frame flows the ordinary way.
    NotAnnounced,
    /// The frame is held behind the durable append.
    Buffered,
    /// The frame was refused and the channel was dropped with it.
    Dropped,
}

/// What [`AnnouncedBarrier::commit`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The buffered frames were drained in arrival order.
    Drained { frames: usize },
    /// The channel is gone; the semantic-retention path owns this commit.
    NotAnnounced,
    /// The announcement named a different session than the commit did.
    SessionMismatch,
    /// The durable index bound a different session, and the channel was dropped.
    MappingMismatch,
    /// The channel was re-announced mid-drain; the drain abandoned.
    Superseded,
}

/// Counters for diagnostics and the `1009` decision the socket layer makes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BarrierStats {
    /// Channels currently announced.
    pub channels: usize,
    /// Frames held across every announced channel.
    pub frames: usize,
    /// Bytes held across every announced channel.
    pub bytes: u64,
    /// Channels still waiting for their durable route.
    pub pending: usize,
    /// Channels draining.
    pub draining: usize,
}

/// The socket-wide retention budget this barrier charges before it retains.
///
/// The budget is the *socket's*, not the channel's, and it is shared with the
/// ordered frame queue: a worker that opens many channels at once must be able
/// to exhaust one budget rather than each channel holding its own. The source
/// wires them together explicitly
/// (`apps/coord/src/workers/worker-ws-upgrade.ts:148`), and the unit tests
/// construct the barrier with a budget for the same reason.
#[derive(Debug, Default)]
pub struct RetainedWorkBudget {
    frames: usize,
    bytes: u64,
    max_frames: usize,
    max_bytes: u64,
}

/// Why a retention was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainOutcome {
    /// The bytes are now charged to the budget.
    Retained,
    /// The budget is full; the caller must release what it holds and drop.
    Refused,
    /// The budget is closed, so a socket that has already overflowed retains
    /// nothing further.
    Closed,
}

impl RetainedWorkBudget {
    /// A budget with the socket-wide bounds.
    ///
    /// The frame and byte caps are the same ones the ordered frame queue uses
    /// (`apps/coord/src/workers/worker-frame-queue.ts:5-6`): 256 frames and
    /// 16 MiB per socket. Charging the barrier against the same budget is what
    /// makes "256 frames" a real statement about one connection rather than a
    /// per-owner one.
    #[must_use]
    pub fn new(max_frames: usize, max_bytes: u64) -> Self {
        Self {
            frames: 0,
            bytes: 0,
            max_frames,
            max_bytes,
        }
    }

    /// Charge `bytes` before retaining anything.
    pub fn retain(&mut self, bytes: u64) -> RetainOutcome {
        if self.is_closed() {
            return RetainOutcome::Closed;
        }
        if self.frames + 1 > self.max_frames || self.bytes + bytes > self.max_bytes {
            return RetainOutcome::Refused;
        }
        self.frames += 1;
        self.bytes += bytes;
        RetainOutcome::Retained
    }

    /// Give `bytes` back after processing or delivery settled.
    pub fn release(&mut self, bytes: u64) {
        self.frames = self.frames.saturating_sub(1);
        self.bytes = self.bytes.saturating_sub(bytes);
    }

    /// Close the budget. Latched: a socket that has overflowed retains nothing
    /// further, so a caller cannot keep charging after the 1009.
    pub fn close(&mut self) {
        self.frames = usize::MAX;
        self.bytes = u64::MAX;
    }

    /// Whether the budget is latched closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.frames == usize::MAX
    }
}
