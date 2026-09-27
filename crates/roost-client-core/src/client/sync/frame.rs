//! One frame, and the metadata a reconnecting client needs to place it.
//!
//! This is the client half of coordinator contract §12.8. There, a frame was
//! queued still holding `SyncFrameMeta::default()` — the enqueue path built the
//! meta, used it to decide where the frame went, and then threw it away. Every
//! consumer then read identity back off the queued item, found none, returned on
//! its first line, and the cursor never advanced.
//!
//! The client half of that is a frame that reaches the queue with no delivery
//! sequence: nothing can acknowledge it, nothing can place it, and the recovery
//! cursor stops where the queue began. So the meta is built ONCE, in the
//! constructor, from the values the socket carried — never defaulted, never
//! filled in later, and never reconstructed from the frame shape — and a frame
//! that arrives without it is refused at the door rather than queued
//! unplaceable.
//!
//! Depends on `sync::inbound::SyncFrame` for the frame vocabulary and on the
//! store's own `is_control`, so "is this an application frame" is asked once.

use crate::event::ClientEvent;
use crate::handle_sync::is_control;
use crate::sync::{SyncDomain, SyncFrame};

/// The lane a frame belongs to: which domain, and which session inside it.
///
/// Derived once, in [`QueuedFrame::new`], and stored beside the frame rather
/// than recomputed at each use — a lane recomputed from a frame that a later
/// match arm forgets to extend is a lane that silently changes meaning. `None`
/// for either field means the frame does not name one: controls ride the
/// negotiation rather than a domain, and a session event belongs to whichever
/// domain the host established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameLane {
    /// The domain, when the frame is domain-bound.
    pub domain: Option<SyncDomain>,
    /// The session, when the frame names one.
    pub session_id: Option<String>,
}

/// One decoded frame plus the transport meta that arrived beside it.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedFrame {
    frame: SyncFrame,
    lane: FrameLane,
    generation: u64,
    delivery_seq: u64,
    socket_id: String,
}

impl QueuedFrame {
    /// Take one frame off a socket, with the meta the wire carried beside it.
    ///
    /// Every argument is required, and none has a default: a caller that has
    /// lost the sequence or the socket id has a decode problem, and the way to
    /// find out is to be unable to say so here.
    pub fn new(
        generation: u64,
        delivery_seq: u64,
        socket_id: impl Into<String>,
        frame: SyncFrame,
    ) -> Self {
        let lane = FrameLane {
            domain: frame.domain(),
            session_id: frame.session_id().map(str::to_owned),
        };
        Self {
            frame,
            lane,
            generation,
            delivery_seq,
            socket_id: socket_id.into(),
        }
    }

    /// The frame, as the host decoded it.
    pub fn frame(&self) -> &SyncFrame {
        &self.frame
    }

    /// The lane this frame was placed in when it arrived.
    pub fn lane(&self) -> &FrameLane {
        &self.lane
    }

    /// The socket generation the frame arrived on. Deliberately NOT a dispatch
    /// gate: reconnecting cannot revoke a frame that was accepted earlier, and
    /// the coordinator will not send it again
    /// (`docs/phase4-client-contract.md` §7).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The transport sequence, or `0` for a control.
    pub fn delivery_seq(&self) -> u64 {
        self.delivery_seq
    }

    /// The coordinator's id for the socket this frame arrived on, which is what
    /// a cumulative acknowledgement is addressed to.
    pub fn socket_id(&self) -> &str {
        &self.socket_id
    }

    /// Whether this frame carries everything a reconnecting client needs to
    /// place it.
    ///
    /// One rule, checked from both ends of the same source: an APPLICATION frame
    /// needs a positive delivery sequence and a control needs none. A control
    /// that carries a sequence is still placeable — the sequence is simply never
    /// acknowledged, because a control has no window cost. An application frame
    /// with sequence `0` is the §12.8 shape: it was never sequenced, so nothing
    /// downstream can place or acknowledge it, and queueing it would hide that
    /// until the cursor stopped.
    pub fn is_placeable(&self) -> bool {
        if is_control(&self.frame) {
            return true;
        }
        self.delivery_seq > 0
    }

    /// The event that applies this frame to the store.
    ///
    /// Consuming, because a frame is applied exactly once and a queue that
    /// handed the same frame out twice would fold one event twice.
    pub fn into_event(self) -> ClientEvent {
        ClientEvent::SyncFrameReceived {
            generation: self.generation,
            delivery_seq: self.delivery_seq,
            frame: self.frame,
        }
    }
}
