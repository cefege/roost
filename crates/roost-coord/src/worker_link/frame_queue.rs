//! The per-socket ordered frame queue: one bounded FIFO that preserves arrival
//! order, because the runtime dispatches messages in order and does not await
//! async handlers.
//!
//! Owned by `worker_link::connection`. Pure — no socket, no clock — so the two
//! properties worth testing (order, and the budget refusing the work) are
//! observable without a live link.
//!
//! THE BUDGET STAYS CHARGED WHILE THE HANDLER SETTLES, which is why a frame's
//! bytes are released by [`FrameQueue::release`] rather than on dequeue. A
//! queue that forgot work as soon as it handed it over would admit twice its
//! bound while the first batch was still in flight, which is exactly the burst
//! the bound exists to refuse.
//!
//! OVERFLOW LATCHES BEFORE IT REFUSES. [`FrameQueue::push`] returns
//! [`QueueRefusal::Overflow`] and marks the queue latched, so every later push
//! is refused the same way without re-charging; the caller closes the socket
//! once, and a worker that keeps sending during the close cannot keep growing
//! the queue it is being closed for overflowing.

use crate::worker_link::retained_budget::{RetainOutcome, RetainedWorkBudget};

/// The frame ceiling, 256 (`worker-frame-queue.ts:5`).
pub const WORKER_FRAME_QUEUE_MAX_FRAMES: usize = 256;

/// The byte ceiling, 16 MiB (`worker-frame-queue.ts:6`).
pub const WORKER_FRAME_QUEUE_MAX_BYTES: usize = 16 * 1024 * 1024;

/// One queued frame and the size charged for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedFrame {
    /// The frame's payload, exactly as it arrived.
    pub payload: Vec<u8>,
    /// The size charged against the byte budget, which is the payload plus the
    /// frame header. The header is counted because it occupies the socket too:
    /// a frame count of 256 empty frames is not free.
    pub charged_bytes: usize,
}

impl QueuedFrame {
    /// A frame and what it costs the budget.
    #[must_use]
    pub fn new(payload: Vec<u8>, header_bytes: usize) -> Self {
        Self {
            charged_bytes: payload.len() + header_bytes,
            payload,
        }
    }
}

/// Why a frame was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueRefusal {
    /// The frame ceiling or the byte ceiling is full. Latches the queue.
    Overflow,
    /// The queue is already latched, so this frame was never considered.
    Latched,
}

/// What the queue did with a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queued {
    /// Admitted, with the queue's depth after it.
    Admitted { depth: usize },
    /// Refused, with the reason. Nothing was charged.
    Refused(QueueRefusal),
}

/// One socket's ordered, bounded, charged queue.
///
/// The bounds are the socket's [`RetainedWorkBudget`], which this queue owns
/// and lends to the announced-channel barrier (`budget_mut`): v2 binds the
/// queue's budget into the barrier (`worker-ws-handler.ts:143`), so the 256
/// frames and 16 MiB are one socket's total, not one owner's.
#[derive(Debug)]
pub struct FrameQueue {
    frames: std::collections::VecDeque<QueuedFrame>,
    budget: RetainedWorkBudget,
}

impl FrameQueue {
    /// A queue at the contract's two bounds.
    #[must_use]
    pub fn new() -> Self {
        Self::with_bounds(WORKER_FRAME_QUEUE_MAX_FRAMES, WORKER_FRAME_QUEUE_MAX_BYTES)
    }

    /// A queue at bounds of the caller's choosing, for a test that must not
    /// allocate 16 MiB to prove a ceiling.
    #[must_use]
    pub fn with_bounds(max_frames: usize, max_bytes: usize) -> Self {
        Self {
            frames: std::collections::VecDeque::with_capacity(max_frames.min(64)),
            budget: RetainedWorkBudget::with_bounds(max_frames, max_bytes as u64),
        }
    }

    /// Admit one frame at the back, preserving arrival order.
    ///
    /// A frame of zero charged bytes is refused rather than admitted: it would
    /// occupy a queue slot forever and charge nothing, so a peer sending empty
    /// frames could hold every slot without ever reaching the byte bound.
    pub fn push(&mut self, frame: QueuedFrame) -> Queued {
        if !self.budget.is_open() {
            return Queued::Refused(QueueRefusal::Latched);
        }
        if frame.charged_bytes == 0 {
            self.latch();
            return Queued::Refused(QueueRefusal::Overflow);
        }
        match self.budget.retain(frame.charged_bytes as u64) {
            RetainOutcome::Retained => {}
            RetainOutcome::Closed => return Queued::Refused(QueueRefusal::Latched),
            RetainOutcome::Overflow => return Queued::Refused(QueueRefusal::Overflow),
        }
        self.frames.push_back(frame);
        Queued::Admitted {
            depth: self.frames.len(),
        }
    }

    /// Take the oldest frame, still charging its bytes until [`Self::release`].
    pub fn take_front(&mut self) -> Option<QueuedFrame> {
        self.frames.pop_front()
    }

    /// Release the bytes of a frame taken by [`Self::take_front`], once its
    /// handler has settled.
    pub fn release(&mut self, frame: &QueuedFrame) {
        self.budget.release(frame.charged_bytes as u64);
    }

    /// Mark the queue closed to further work, and report whether this call is
    /// the one that latched it.
    ///
    /// The bool exists so the caller logs the FIRST overflow once and does not
    /// log a line per refused frame afterwards, which is how an overflow turns
    /// into a log flood that hides the one line that mattered.
    pub fn latch(&mut self) -> bool {
        let was_open = self.budget.is_open();
        self.budget.close();
        was_open
    }

    /// Whether the queue has latched.
    #[must_use]
    pub fn is_latched(&self) -> bool {
        !self.budget.is_open()
    }

    /// Frames still held, which is the depth the budget is checked against.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// Bytes still charged by every owner on the socket, including frames
    /// whose handlers have not settled.
    #[must_use]
    pub fn charged_bytes(&self) -> usize {
        usize::try_from(self.budget.stats().bytes).unwrap_or(usize::MAX)
    }

    /// The socket's shared budget, lent to every other retention owner.
    pub fn budget_mut(&mut self) -> &mut RetainedWorkBudget {
        &mut self.budget
    }

    /// The socket's shared budget, read-only.
    #[must_use]
    pub fn budget(&self) -> &RetainedWorkBudget {
        &self.budget
    }
}

impl Default for FrameQueue {
    fn default() -> Self {
        Self::new()
    }
}
