//! The coordinator link's outbox: the lanes, their caps, and the order they
//! drain in. Owned by the worker.
//!
//! The whole point of this file is ONE ordering rule, and everything else is
//! in service of it: **a session's `opened` event must reach the coordinator
//! before that session's first terminal frame.** A cell frame for a session
//! nobody has been told about is a frame the browser cannot place, and the
//! failure looks like a terminal that never paints rather than like an
//! ordering bug.
//!
//! So terminal frames are drained LAST, behind everything durable and
//! everything that controls them. That is a deliberate choice to let a backlog
//! of cells wait — a late frame is still correct, a frame ahead of its own
//! `opened` is not recoverable.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How many frames may be waiting across every lane.
pub const PENDING_CAP: usize = 1024;

/// How many bytes may be waiting across every lane.
///
/// Frames are ENCODED before admission rather than measured as objects, so the
/// cap is exact. Estimating over a mutable proto and discovering at send time
/// that the queue is 40% over its limit is how a byte cap stops being one.
pub const PENDING_BYTES_CAP: usize = 8 * 1024 * 1024;

/// Raw PTY bytes feed coordinator-only scanners, and cells normally go first.
///
/// A raw frame that has waited longer than this is PROMOTED ahead of the
/// cells, so a chatty terminal cannot starve the scanners indefinitely. The
/// age bound is short because the scanners run on the coordinator and stale
/// raw bytes are worth less than fresh ones, not more.
pub const RAW_METADATA_MAX_AGE: Duration = Duration::from_millis(100);

/// Which lane a frame belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    /// Session events the coordinator must not lose. Never evicted here: the
    /// session-event store owns that capacity, and evicting a durable event to
    /// make room for a cell would trade a permanent hole for a delayed frame.
    Durable,
    /// Commands and acknowledgements that change what the link does next.
    Control,
    /// Authoritative terminal frames. Drained last, always.
    Terminal,
    /// Raw PTY bytes for coordinator-side scanners. Volatile: this is the only
    /// lane the outbox is allowed to drop from.
    RawMetadata,
}

impl Lane {
    /// Every lane, in drain order.
    ///
    /// The order IS the contract. `Terminal` is last because a cell ahead of
    /// its own `opened` is unrecoverable, while a cell behind one is merely
    /// late.
    pub const DRAIN_ORDER: [Lane; 4] = [
        Lane::Durable,
        Lane::Control,
        Lane::Terminal,
        Lane::RawMetadata,
    ];

    /// Whether this lane may be dropped when the caps are reached.
    ///
    /// Only raw metadata. Dropping anything else loses information the
    /// coordinator cannot reconstruct: a durable event is a fact about what
    /// happened, and a terminal frame is the only description of the screen.
    pub fn is_droppable(self) -> bool {
        self == Lane::RawMetadata
    }
}

/// A frame waiting to go upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub lane: Lane,
    /// The encoded frame. Encoding happens at admission so the byte cap is
    /// exact.
    pub bytes: Vec<u8>,
    pub queued_at: Instant,
    /// What the frame is, for logs and for tests that assert on ordering.
    pub label: String,
}

impl Pending {
    /// How long this frame has been waiting.
    pub fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.queued_at)
    }
}

/// Why a frame was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdmitError {
    #[error("the outbox is full: {pending} frames waiting against a cap of {cap}")]
    Full { pending: usize, cap: usize },
    #[error(
        "the outbox holds {bytes} bytes against a cap of {cap}, and this lane may not be dropped"
    )]
    OverBytes { bytes: usize, cap: usize },
    #[error("this frame encodes to {bytes} bytes, which alone exceeds the {cap} byte cap")]
    FrameTooLarge { bytes: usize, cap: usize },
}

/// What admitting a frame did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admitted {
    /// The frame is waiting.
    Queued,
    /// The lane was full and the frame was dropped because it may be.
    DroppedVolatile,
}

/// The outbox.
#[derive(Debug)]
pub struct Outbox {
    lanes: [VecDeque<Pending>; 4],
    frame_count: usize,
    byte_count: usize,
    cap: usize,
    byte_cap: usize,
}

impl Default for Outbox {
    fn default() -> Self {
        Self::new(PENDING_CAP, PENDING_BYTES_CAP)
    }
}

impl Outbox {
    pub fn new(cap: usize, byte_cap: usize) -> Self {
        Self {
            lanes: [
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
            ],
            frame_count: 0,
            byte_count: 0,
            cap,
            byte_cap,
        }
    }

    fn slot(lane: Lane) -> usize {
        match lane {
            Lane::Durable => 0,
            Lane::Control => 1,
            Lane::RawMetadata => 2,
            Lane::Terminal => 3,
        }
    }

    pub fn frame_count(&self) -> usize {
        self.frame_count
    }

    pub fn byte_count(&self) -> usize {
        self.byte_count
    }

    /// How many frames a lane is holding.
    pub fn lane_len(&self, lane: Lane) -> usize {
        self.lanes[Self::slot(lane)].len()
    }

    pub fn is_empty(&self) -> bool {
        self.frame_count == 0
    }

    /// Offer an already-encoded frame.
    ///
    /// A lane that may be dropped is dropped rather than refusing; one that may
    /// not is refused, so the caller learns that its durable event did not fit
    /// instead of discovering later that the coordinator never heard about it.
    pub fn admit(
        &mut self,
        lane: Lane,
        bytes: Vec<u8>,
        label: impl Into<String>,
        now: Instant,
    ) -> Result<Admitted, AdmitError> {
        if bytes.len() > self.byte_cap {
            return Err(AdmitError::FrameTooLarge {
                bytes: bytes.len(),
                cap: self.byte_cap,
            });
        }

        // A droppable lane sheds its OLDEST frame rather than the new one. The
        // new frame is the one the caller is holding a reference to, and
        // dropping it would make the caller's accounting a lie.
        if self.frame_count >= self.cap || self.byte_count + bytes.len() > self.byte_cap {
            if !lane.is_droppable() {
                return Err(if self.frame_count >= self.cap {
                    AdmitError::Full {
                        pending: self.frame_count,
                        cap: self.cap,
                    }
                } else {
                    AdmitError::OverBytes {
                        bytes: self.byte_count,
                        cap: self.byte_cap,
                    }
                });
            }
            if let Some(evicted) = self.lanes[Self::slot(lane)].pop_front() {
                self.frame_count -= 1;
                self.byte_count -= evicted.bytes.len();
            }
        }

        let frame = Pending {
            lane,
            bytes,
            queued_at: now,
            label: label.into(),
        };
        self.byte_count += frame.bytes.len();
        self.frame_count += 1;
        self.lanes[Self::slot(lane)].push_back(frame);
        Ok(Admitted::Queued)
    }

    /// Take the next frame, in the order the contract requires.
    ///
    /// `now` decides whether a raw-metadata frame has aged into promotion; the
    /// clock is a parameter so the promotion rule is testable without sleeping.
    pub fn drain_one(&mut self, now: Instant) -> Option<Pending> {
        // A raw frame past its age bound goes first even though its lane sits
        // ahead of Terminal, or a chatty terminal starves the scanners.
        if let Some(front) = self.lanes[Self::slot(Lane::RawMetadata)].front()
            && front.age(now) >= RAW_METADATA_MAX_AGE
        {
            return self.take_from(Lane::RawMetadata);
        }
        for lane in Lane::DRAIN_ORDER {
            if let Some(frame) = self.take_from(lane) {
                return Some(frame);
            }
        }
        None
    }

    fn take_from(&mut self, lane: Lane) -> Option<Pending> {
        let frame = self.lanes[Self::slot(lane)].pop_front()?;
        self.frame_count -= 1;
        self.byte_count -= frame.bytes.len();
        Some(frame)
    }

    /// Take everything, in drain order. What a reconnect replays.
    pub fn drain_all(&mut self, now: Instant) -> Vec<Pending> {
        let mut drained = Vec::with_capacity(self.frame_count);
        while let Some(frame) = self.drain_one(now) {
            drained.push(frame);
        }
        drained
    }

    /// Drop everything in one lane, oldest first. Used when a stream generation
    /// is invalidated and its pending cells are meaningless.
    pub fn discard(&mut self, lane: Lane) -> usize {
        let dropped = self.lanes[Self::slot(lane)].len();
        while let Some(frame) = self.take_from(lane) {
            let _ = frame;
        }
        dropped
    }
}
