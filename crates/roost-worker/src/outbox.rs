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
//! Ports v2 `apps/worker/src/transport/coord-link-constants.ts`, `apps/worker/src/transport/coord-link-outbox.ts`.

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

/// Which lane a frame belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    /// v2 `livenessPending`: the pong. Written ahead of everything, and before
    /// the barrier is live, so a coordinator's ping is answered while replay runs.
    Liveness,
    /// Session events the coordinator must not lose. Never evicted here: the
    /// session-event store owns that capacity, and evicting a durable event to
    /// make room for a cell would trade a permanent hole for a delayed frame.
    Durable,
    /// Commands and acknowledgements that change what the link does next.
    Control,
    /// Authoritative terminal frames. Drained last, always.
    Terminal,
}

impl Lane {
    /// Every lane, in drain order.
    ///
    /// The order IS the contract. `Terminal` is last because a cell ahead of
    /// its own `opened` is unrecoverable, while a cell behind one is merely
    /// late.
    pub const DRAIN_ORDER: [Lane; 4] =
        [Lane::Liveness, Lane::Durable, Lane::Control, Lane::Terminal];
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
    /// The key this frame supersedes, for a producer whose newest record
    /// replaces its own previous one. `None` for every frame that accumulates.
    pub coalesce_key: Option<String>,
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
    #[error("the outbox holds {bytes} bytes against a cap of {cap}")]
    OverBytes { bytes: usize, cap: usize },
    #[error("this frame encodes to {bytes} bytes, which alone exceeds the {cap} byte cap")]
    FrameTooLarge { bytes: usize, cap: usize },
    /// This frame's payload alone would push the outbox past its byte cap once
    /// a coalesced frame's predecessor is discounted.
    ///
    /// Distinct from [`AdmitError::OverBytes`], which is about what the outbox
    /// already holds. Here the frame fits on its own and does not fit beside
    /// the one it would replace, and the truthful answer is the same: the newer
    /// record is refused and the older one stays, because a dropped title is a
    /// stale title and a refused one is a recorded refusal.
    #[error(
        "this frame is {incoming} bytes and supersedes one of {superseded}, over the {cap} byte cap"
    )]
    CoalesceOverBytes {
        incoming: usize,
        superseded: usize,
        cap: usize,
    },
}

/// What admitting a frame did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admitted {
    /// The frame is waiting, having added to its lane.
    Queued,
    /// The frame replaced this lane's frame for its own key, in place.
    ///
    /// A distinct answer from [`Admitted::Queued`] because nothing was added:
    /// the lane's length is unchanged, and a caller counting what it added is
    /// counting the wrong thing if the two are one value.
    Coalesced,
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
            Lane::Terminal => 2,
            Lane::Liveness => 3,
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
    /// A frame that does not fit is refused, so the caller learns that its
    /// durable event did not fit instead of discovering later that the
    /// coordinator never heard about it.
    pub fn admit(
        &mut self,
        lane: Lane,
        bytes: Vec<u8>,
        label: impl Into<String>,
        now: Instant,
    ) -> Result<Admitted, AdmitError> {
        self.insert(lane, bytes, label, now, None)
    }

    /// Take the next frame, in the order the contract requires.
    pub fn drain_one(&mut self) -> Option<Pending> {
        for lane in Lane::DRAIN_ORDER {
            if let Some(frame) = self.take_from(lane) {
                return Some(frame);
            }
        }
        None
    }

    /// Take the oldest frame of one lane, whatever the drain order says.
    pub fn take_from(&mut self, lane: Lane) -> Option<Pending> {
        let frame = self.lanes[Self::slot(lane)].pop_front()?;
        self.frame_count -= 1;
        self.byte_count -= frame.bytes.len();
        Some(frame)
    }

    /// Take everything, in drain order. What a reconnect replays.
    pub fn drain_all(&mut self) -> Vec<Pending> {
        let mut drained = Vec::with_capacity(self.frame_count);
        while let Some(frame) = self.drain_one() {
            drained.push(frame);
        }
        drained
    }

    /// Offer a frame that SUPERSEDES this lane's frame for the same key.
    ///
    /// The outbox holds ONE frame per key: the newer record replaces the older
    /// one where it sits, so the lane's order is unchanged and its length is
    /// unchanged. A volatile terminal title is replaced by its successor, so a
    /// link applying backpressure must not accumulate every version and then ship
    /// them all in order, which spends the bandwidth AND walks the coordinator
    /// through a replacement edge it has already passed.
    ///
    /// The replacement scans the lane rather than looking a key up in a map. A
    /// coalescing lane holds one entry per key, and a machine has a bounded
    /// number of sessions, so this is a walk over a handful of short strings on
    /// a path that runs per status change rather than per cell frame.
    pub fn admit_coalescing(
        &mut self,
        key: &str,
        lane: Lane,
        bytes: Vec<u8>,
        label: impl Into<String>,
        now: Instant,
    ) -> Result<Admitted, AdmitError> {
        self.refuse_oversized(&bytes)?;
        let slot = Self::slot(lane);
        if let Some(position) = self.lanes[slot]
            .iter()
            .position(|frame| frame.coalesce_key.as_deref() == Some(key))
        {
            let superseded = self.lanes[slot][position].bytes.len();
            if self.byte_count - superseded + bytes.len() > self.byte_cap {
                return Err(AdmitError::CoalesceOverBytes {
                    incoming: bytes.len(),
                    superseded,
                    cap: self.byte_cap,
                });
            }
            self.byte_count = self.byte_count - superseded + bytes.len();
            let frame = &mut self.lanes[slot][position];
            frame.bytes = bytes;
            frame.queued_at = now;
            frame.label = label.into();
            return Ok(Admitted::Coalesced);
        }
        self.insert(lane, bytes, label, now, Some(key.to_owned()))
    }

    /// Whether this lane is holding a frame for `key`.
    pub fn coalesces(&self, lane: Lane, key: &str) -> bool {
        self.lanes[Self::slot(lane)]
            .iter()
            .any(|frame| frame.coalesce_key.as_deref() == Some(key))
    }

    /// Admit a frame, applying the caps. Every lane path ends here, so the cap
    /// arithmetic exists once rather than once per kind of frame.
    fn insert(
        &mut self,
        lane: Lane,
        bytes: Vec<u8>,
        label: impl Into<String>,
        now: Instant,
        coalesce_key: Option<String>,
    ) -> Result<Admitted, AdmitError> {
        self.refuse_oversized(&bytes)?;
        if self.frame_count >= self.cap {
            return Err(AdmitError::Full {
                pending: self.frame_count,
                cap: self.cap,
            });
        }
        if self.byte_count + bytes.len() > self.byte_cap {
            return Err(AdmitError::OverBytes {
                bytes: self.byte_count,
                cap: self.byte_cap,
            });
        }

        let frame = Pending {
            lane,
            bytes,
            queued_at: now,
            label: label.into(),
            coalesce_key,
        };
        self.byte_count += frame.bytes.len();
        self.frame_count += 1;
        self.lanes[Self::slot(lane)].push_back(frame);
        Ok(Admitted::Queued)
    }

    /// Refuse a frame that alone exceeds the byte cap, on every admit path: no
    /// amount of draining makes room for a frame larger than the whole outbox.
    fn refuse_oversized(&self, bytes: &[u8]) -> Result<(), AdmitError> {
        if bytes.len() > self.byte_cap {
            return Err(AdmitError::FrameTooLarge {
                bytes: bytes.len(),
                cap: self.byte_cap,
            });
        }
        Ok(())
    }

    /// Drop everything in one lane, oldest first. Used when a stream generation
    /// is invalidated and its pending cells are meaningless.
    pub fn discard(&mut self, lane: Lane) -> usize {
        let dropped = self.lanes[Self::slot(lane)].len();
        while self.take_from(lane).is_some() {}
        dropped
    }
}
