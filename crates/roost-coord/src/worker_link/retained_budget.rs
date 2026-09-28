//! The socket-wide retained-work budget every inbound retention owner charges:
//! the ordered backlog, the durable frame whose append is in flight, the frames
//! the announced-channel barrier holds, and the parked semantic metadata.
//!
//! Owned by `worker_link::frame_queue` (one per socket, reached by
//! `result_lane` and the announced lane). Ports `WorkerRetainedWorkBudget` of
//! `apps/coord/src/workers/worker-frame-queue.ts`: charge before retaining,
//! release only once processing or delivery settled, and an overflow latches
//! the budget closed before anything else can be charged.

use crate::worker_link::frame_queue::{
    WORKER_FRAME_QUEUE_MAX_BYTES, WORKER_FRAME_QUEUE_MAX_FRAMES,
};

/// What one socket holds right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetStats {
    /// Frames charged, in flight included.
    pub frames: usize,
    /// Bytes charged, in flight included.
    pub bytes: u64,
}

/// The charge that latched the budget: what was held, and what was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetOverflow {
    /// Frames held when the charge was refused.
    pub frames: usize,
    /// Bytes held when the charge was refused.
    pub bytes: u64,
    /// The size of the refused charge.
    pub rejected_bytes: u64,
}

/// What [`RetainedWorkBudget::retain`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainOutcome {
    /// Charged; the caller owes one `release` of the same size.
    Retained,
    /// Already latched; nothing was charged.
    Closed,
    /// This charge crossed a bound and latched the budget; nothing was charged.
    Overflow,
}

/// One socket's shared budget.
#[derive(Debug)]
pub struct RetainedWorkBudget {
    stats: BudgetStats,
    max_frames: usize,
    max_bytes: u64,
    open: bool,
    overflow: Option<BudgetOverflow>,
}

impl RetainedWorkBudget {
    /// A budget at the socket's two bounds, 256 frames and 16 MiB.
    #[must_use]
    pub fn new() -> Self {
        Self::with_bounds(
            WORKER_FRAME_QUEUE_MAX_FRAMES,
            WORKER_FRAME_QUEUE_MAX_BYTES as u64,
        )
    }

    /// A budget at bounds of the caller's choosing.
    #[must_use]
    pub fn with_bounds(max_frames: usize, max_bytes: u64) -> Self {
        Self {
            stats: BudgetStats::default(),
            max_frames,
            max_bytes,
            open: true,
            overflow: None,
        }
    }

    /// Charge one frame of `bytes` before retaining it.
    pub fn retain(&mut self, bytes: u64) -> RetainOutcome {
        if !self.open {
            return RetainOutcome::Closed;
        }
        if self.stats.frames >= self.max_frames
            || bytes > self.max_bytes.saturating_sub(self.stats.bytes)
        {
            self.open = false;
            self.overflow = Some(BudgetOverflow {
                frames: self.stats.frames,
                bytes: self.stats.bytes,
                rejected_bytes: bytes,
            });
            return RetainOutcome::Overflow;
        }
        self.stats.frames += 1;
        self.stats.bytes += bytes;
        RetainOutcome::Retained
    }

    /// Give back one settled frame's charge.
    ///
    /// v2 throws on an underflow; here it is a logged no-op, because a release
    /// larger than what is held means an owner double-released and the charge
    /// it would corrupt belongs to every other owner on the socket.
    pub fn release(&mut self, bytes: u64) {
        if self.stats.frames == 0 || bytes > self.stats.bytes {
            tracing::error!(
                frames = self.stats.frames,
                held_bytes = self.stats.bytes,
                released_bytes = bytes,
                "worker link: retained-work budget underflow; the release is ignored"
            );
            return;
        }
        self.stats.frames -= 1;
        self.stats.bytes -= bytes;
    }

    /// Latch the budget closed without an overflow: the socket is ending.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Whether anything more may be charged.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// What is charged right now.
    #[must_use]
    pub fn stats(&self) -> BudgetStats {
        self.stats
    }

    /// The charge that latched the budget, when an overflow is what closed it.
    #[must_use]
    pub fn overflow(&self) -> Option<BudgetOverflow> {
        self.overflow
    }
}

impl Default for RetainedWorkBudget {
    fn default() -> Self {
        Self::new()
    }
}
