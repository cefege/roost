//! The host's queue between a socket and the store.
//!
//! A browser socket delivers into a callback, not a stream, so something has to
//! be a queue. This is it, and its whole contract is one sentence: a frame
//! enters WITH its meta or not at all. It is the client half of coordinator
//! contract §12.8, and it is the reason [`crate::client::sync::frame::QueuedFrame`]
//! has no `Default` and no half-built constructor.
//!
//! The queue is deliberately NOT the store's pre-hydration queue. That one
//! (`SyncState::retain`) holds frames the core cannot apply yet, and a new
//! generation clears it because the coordinator re-sends those records above the
//! new socket's recovery cutoff. This one holds frames the host has decoded and
//! not yet reported, which is a different interval: it must SURVIVE a close and a
//! redial, because the frame is already off the wire and nothing will send it
//! again.

use std::collections::{BTreeSet, VecDeque};

use crate::client::sync::frame::QueuedFrame;

/// The most frames held before the oldest is dropped.
///
/// The coordinator's application window is 512 records
/// (`crates/roost-coord/src/sync_ws/ack_window.rs:33`) and the platform's inbox
/// is the same number, and this is the same number for the same reason: a queue
/// at the window's size means the window is full, so the next frame would have
/// been refused by the coordinator anyway. A dropped frame is recoverable by a
/// resync; an unbounded queue in a backgrounded tab is not recoverable at all.
pub const SYNC_DISPATCH_QUEUE_MAX: usize = 512;

/// A frame that was refused rather than queued, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnplaceableFrame {
    /// The generation it claimed to arrive on.
    pub generation: u64,
    /// The sequence it carried, which is what was missing.
    pub delivery_seq: u64,
    /// Which frame kind it was, for the incident log.
    pub kind: &'static str,
}

impl UnplaceableFrame {
    /// The sentence a log line reads, naming the missing value.
    pub fn reason(&self) -> &'static str {
        "an application frame arrived with no delivery sequence, so nothing can place or acknowledge it"
    }
}

/// What became of one frame offered to the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// Queued.
    Queued,
    /// Queued, and the oldest held frame was dropped to make room.
    QueuedEvictingOldest,
    /// Refused. Nothing was queued, and the frame is gone.
    Refused(UnplaceableFrame),
}

/// The host's queue between a socket and the store.
#[derive(Debug, Default)]
pub struct SyncDispatch {
    queue: VecDeque<QueuedFrame>,
}

impl SyncDispatch {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer one frame, with the meta the socket carried.
    ///
    /// A frame that cannot be placed is REFUSED here and never enters the queue.
    /// That is the whole §12.8 rule on this side: the alternative — queue it and
    /// let the consumer discover it has nothing — is what made a multi-part
    /// baseline stop after part one, invisibly, on a coordinator that was
    /// behaving correctly.
    pub fn enqueue(&mut self, frame: QueuedFrame) -> EnqueueOutcome {
        if !frame.is_placeable() {
            return EnqueueOutcome::Refused(UnplaceableFrame {
                generation: frame.generation(),
                delivery_seq: frame.delivery_seq(),
                kind: frame.frame().kind_name(),
            });
        }
        if self.queue.len() >= SYNC_DISPATCH_QUEUE_MAX {
            self.queue.pop_front();
            self.queue.push_back(frame);
            return EnqueueOutcome::QueuedEvictingOldest;
        }
        self.queue.push_back(frame);
        EnqueueOutcome::Queued
    }

    /// Take everything held, in arrival order, with its meta intact.
    ///
    /// Draining, not copying: the coordinator sequences these frames, and the
    /// order it sequenced them in is the order they must be applied in. A queue
    /// that peeked would let a caller apply the same frame twice.
    pub fn take(&mut self) -> Vec<QueuedFrame> {
        self.queue.drain(..).collect()
    }

    /// How many frames are held.
    pub fn held(&self) -> usize {
        self.queue.len()
    }

    /// Whether nothing is held.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The generations still represented in the queue.
    ///
    /// A diagnostic and a fence in one: a host that redials can report which
    /// generations it is still carrying, and a generation that is NOT in here
    /// has nothing left to place.
    pub fn generations_held(&self) -> BTreeSet<u64> {
        self.queue.iter().map(QueuedFrame::generation).collect()
    }

    /// Drop everything held, and say how many frames that was.
    ///
    /// For a credential boundary, where the queue is keyed to the credential that
    /// just went away and every frame in it is about to be re-sent by a
    /// coordinator that will not recognise this client.
    pub fn clear(&mut self) -> usize {
        let held = self.queue.len();
        self.queue.clear();
        held
    }
}
