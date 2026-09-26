//! One frame as a single Sync socket owns it: its payload, its byte estimate,
//! and the per-recipient scalars that are stamped on at send time.
//!
//! Owned by the Sync session's queues. The important decision here is that a
//! canonical terminal full is held as an `Arc` and COPIED only when it is
//! stamped for one recipient, because that is what makes a five-viewer session
//! cost one grid in coordinator memory instead of five. v2 reached the same
//! shape with a hand-written "shallow message shell" (`sync-ws-v2-terminal-
//! payload.ts:38-44`) that shared the row and span arrays between recipients;
//! `Arc` is the same sharing with the aliasing rule enforced by the compiler.
//!
//! WHY THE SEND-TIME COPY IS NOT WASTED. The alternative -- a hand-rolled
//! partial encoder for the two cell oneof cases -- would be a second
//! implementation of the wire format, which this repository forbids. The copy
//! is one pass over a structure the encoder is about to walk anyway, and
//! retention, which is where a queue actually grows, stays O(1) per socket.

use std::sync::Arc;

use roost_proto::buffa::Message;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{FirehoseFrame, PbCellGridChunk, PbCellGridFrame, SyncDomain};
use roost_protocol::cell::frame_chunk_validation::CELL_GRID_COORD_FANOUT_STAMP_MAX;

use super::frame_meta::SyncFrameMeta;

/// The terminal cell payload several sockets read from one canonical source.
#[derive(Debug, Clone, PartialEq)]
pub enum SharedCellFrame {
    /// A whole grid, when it fitted inside one part.
    Full(PbCellGridFrame),
    /// One part of a chunked baseline.
    Chunk(PbCellGridChunk),
}

impl SharedCellFrame {
    /// The grid this payload is, for a whole frame or a chunk's part.
    fn grid_mut(&mut self) -> Option<&mut PbCellGridFrame> {
        match self {
            Self::Full(grid) => Some(grid),
            Self::Chunk(chunk) => chunk.part.as_option_mut(),
        }
    }

    fn oneof(&self) -> Frame {
        match self {
            Self::Full(grid) => Frame::CellGrid(Box::new(grid.clone())),
            Self::Chunk(chunk) => Frame::CellGridChunk(Box::new(chunk.clone())),
        }
    }
}

/// The bytes a frame's per-recipient scalars can add on top of a measurement
/// taken with them at zero.
///
/// `delivery_seq` is field 31, so its tag is two bytes and a `u64` varint is
/// ten: twelve. v2 charged a flat ten here (`sync-ws-v2-state.ts:148`), which
/// is one byte short of a `u64` sequence; the port charges the exact bound
/// because a byte of headroom here is a byte of queue that never fills.
pub const DELIVERY_SCALAR_HEADROOM_BYTES: u64 = 12;

/// One frame, as this socket holds it, before its send-time scalars are set.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnedFrame {
    payload: OwnedPayload,
    /// The domain and generation this socket claimed at admission. Stamped onto
    /// the wire copy, and compared on re-admission, so a frame retained under a
    /// reset generation can never be queued into the new one.
    domain: SyncDomain,
    generation: u64,
}

#[derive(Debug, Clone, PartialEq)]
enum OwnedPayload {
    /// A frame this socket owns outright, stamped at admission.
    Copy(FirehoseFrame),
    /// A share of the canonical terminal full every viewer of a session reads.
    Shared(Arc<SharedCellFrame>),
}

impl OwnedFrame {
    /// Take a private copy of `frame` for one socket, stamped with the domain
    /// and generation that socket is currently admitting for.
    #[must_use]
    pub fn of_copy(frame: &FirehoseFrame, domain: SyncDomain, generation: u64) -> Self {
        let mut owned = frame.clone();
        stamp_envelope(&mut owned, domain, generation, 0);
        Self {
            payload: OwnedPayload::Copy(owned),
            domain,
            generation,
        }
    }

    /// Share a canonical terminal full with every other recipient.
    #[must_use]
    pub fn of_shared_cell(cell: SharedCellFrame, generation: u64) -> Self {
        Self {
            payload: OwnedPayload::Shared(Arc::new(cell)),
            domain: SyncDomain::Terminal,
            generation,
        }
    }

    /// The domain this frame was admitted for.
    #[must_use]
    pub fn domain(&self) -> SyncDomain {
        self.domain
    }

    /// The generation this frame was admitted under.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether this frame is a terminal view-state.
    ///
    /// The one cell-lane frame that is semantic state rather than cells, and
    /// the one that is not fenced behind a session announcement: it answers a
    /// command this socket sent, so the client already asked about the session
    /// (`sync-ws-v2-state.ts:323-333`).
    #[must_use]
    pub fn is_view_state(&self) -> bool {
        match &self.payload {
            OwnedPayload::Shared(_) => false,
            OwnedPayload::Copy(frame) => {
                matches!(frame.frame, Some(Frame::TerminalViewState(_)))
            }
        }
    }

    /// The session an agent-status frame is about, and `None` for every other
    /// shape. Agent status is the one current-value projection in the feed, so
    /// the queue needs to identify it by session and nothing else does.
    #[must_use]
    pub fn agent_status_session(&self) -> Option<&str> {
        match &self.payload {
            OwnedPayload::Shared(_) => None,
            OwnedPayload::Copy(frame) => match &frame.frame {
                Some(Frame::AgentStatus(status)) => Some(&status.session_id),
                _ => None,
            },
        }
    }

    /// Whether this frame is cell material: a whole grid or one chunk part.
    ///
    /// The one predicate the whole terminal half turns on, because cell
    /// material is droppable under pressure while everything else in the
    /// terminal domain is semantic state a client cannot rebuild
    /// (`sync-ws-v2-state.ts:93-95`).
    #[must_use]
    pub fn is_cell_material(&self) -> bool {
        match &self.payload {
            OwnedPayload::Shared(_) => true,
            OwnedPayload::Copy(frame) => matches!(
                frame.frame,
                Some(Frame::CellGrid(_) | Frame::CellGridChunk(_))
            ),
        }
    }

    /// Whether this frame is a baseline: a chunk part, or a grid marked full.
    #[must_use]
    pub fn is_snapshot(&self) -> bool {
        match &self.payload {
            OwnedPayload::Shared(_) => true,
            OwnedPayload::Copy(frame) => match &frame.frame {
                Some(Frame::CellGridChunk(_)) => true,
                Some(Frame::CellGrid(grid)) => grid.full,
                _ => false,
            },
        }
    }

    /// The bytes this frame charges against its socket's retention budget,
    /// measured with the widest scalars a per-recipient stamp can occupy.
    #[must_use]
    pub fn conservative_bytes(&self) -> u64 {
        match &self.payload {
            OwnedPayload::Copy(frame) => {
                u64::from(frame.encoded_len()) + DELIVERY_SCALAR_HEADROOM_BYTES
            }
            OwnedPayload::Shared(cell) => {
                let mut probe = (**cell).clone();
                if let Some(grid) = probe.grid_mut() {
                    grid.coord_fanout_ms = CELL_GRID_COORD_FANOUT_STAMP_MAX;
                }
                u64::from(shell(&probe, self.domain, self.generation).encoded_len())
            }
        }
    }

    /// The frame as it goes on the wire for one recipient: the delivery
    /// sequence, the envelope scalars, and the fan-out stamp for the moment
    /// this socket flushed it.
    ///
    /// This COPIES, and it copies before the send rather than after, because the
    /// window check has to see the frame that is about to be written. A
    /// retained frame is only copied once per flush turn, and only for the one
    /// recipient being written to.
    ///
    /// `fanout_ms` is the snapshot's own stamp for every part of a chunked
    /// baseline, so a browser can tell how much of its own latency the
    /// coordinator spent queueing the REST of that snapshot
    /// (`sync-ws-v2-terminal-payload.ts:88-110`).
    #[must_use]
    pub fn outbound_copy(&self, delivery_seq: u64, fanout_ms: u64) -> FirehoseFrame {
        let mut frame = match &self.payload {
            OwnedPayload::Copy(frame) => frame.clone(),
            OwnedPayload::Shared(cell) => {
                let mut shared = (**cell).clone();
                match &mut shared {
                    SharedCellFrame::Full(grid) => grid.coord_fanout_ms = fanout_ms,
                    SharedCellFrame::Chunk(chunk) => {
                        if let Some(part) = chunk.part.as_option_mut() {
                            part.coord_fanout_ms = fanout_ms;
                        }
                    }
                }
                shell(&shared, self.domain, self.generation)
            }
        };
        stamp_envelope(&mut frame, self.domain, self.generation, delivery_seq);
        frame
    }
}

/// Wrap a shared cell payload in the envelope every Sync frame travels in.
fn shell(cell: &SharedCellFrame, domain: SyncDomain, generation: u64) -> FirehoseFrame {
    FirehoseFrame {
        delivery_seq: 0,
        domain_generation: generation,
        domain: domain.into(),
        frame: Some(cell.oneof()),
        ..FirehoseFrame::default()
    }
}

fn stamp_envelope(frame: &mut FirehoseFrame, domain: SyncDomain, generation: u64, seq: u64) {
    frame.delivery_seq = seq;
    frame.domain = domain.into();
    frame.domain_generation = generation;
}

/// How long a chunked baseline spent being queued, for one diagnostic line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkTransfer {
    /// The session the part belongs to.
    pub session_id: String,
    /// The snapshot this part is one of.
    pub snapshot_id: String,
    /// This part's index.
    pub chunk_index: u32,
    /// How many parts the baseline has.
    pub chunk_count: u32,
    /// Milliseconds between the snapshot's fan-out stamp and this send.
    pub transfer_ms: u64,
}

/// What a socket's retention budget has been charged for one retained frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggregateCharge {
    /// The frame's conservative size.
    pub estimated_bytes: u64,
    /// Charged against the terminal half of the budget.
    pub terminal: bool,
    /// Charged against the terminal cell sub-budget, which is smaller than the
    /// terminal budget so state frames always have room left.
    pub terminal_cell: bool,
}

/// One frame sitting in a socket's queues.
#[derive(Debug)]
pub struct RetainedFrame {
    /// The frame and its envelope scalars.
    pub frame: OwnedFrame,
    /// Where it came from and how it must be ordered.
    pub meta: SyncFrameMeta,
    /// When the socket queued it, which is what the weighted-lane age rule and
    /// the FIFO tie-breaks read.
    pub queued_at_ms: u64,
    charged_bytes: u64,
    charge: Option<AggregateCharge>,
}

impl RetainedFrame {
    /// Queue a frame with its charge taken.
    pub(in crate::sync_ws) fn new(
        frame: OwnedFrame,
        meta: SyncFrameMeta,
        queued_at_ms: u64,
        charge: AggregateCharge,
    ) -> Self {
        Self {
            frame,
            meta,
            queued_at_ms,
            charged_bytes: charge.estimated_bytes,
            charge: Some(charge),
        }
    }

    /// The bytes this frame is charged.
    ///
    /// Read from the charge rather than re-measured: measuring a shared cell
    /// means cloning the grid, and this is read once per frame released.
    pub(in crate::sync_ws) fn estimated_bytes(&self) -> u64 {
        self.charged_bytes
    }

    /// Give the budget back, once.
    ///
    /// Taking the charge out of the frame is what makes a double release
    /// impossible; v2 flipped a boolean on the charge object, which is the same
    /// rule with a wider blast radius (`sync-ws-v2-state.ts:201-218`).
    pub(in crate::sync_ws) fn take_charge(&mut self) -> Option<AggregateCharge> {
        self.charge.take()
    }
}
