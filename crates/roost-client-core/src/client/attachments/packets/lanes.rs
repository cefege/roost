//! Both lanes' packet state for one attachment peer, so attachment traffic can
//! never borrow a terminal lane's queue or budget. Ported from
//! `attachmentPeerPackets.ts`; the stall decision is a value a host asks about
//! rather than a timer here. Depends on the queue, the assembler and the lane
//! table in the parent module.

use super::assembler::AttachmentPacketAssembler;
use super::queue::AttachmentPacketQueue;
use super::{ATTACHMENT_PEER_LANES, AttachmentPacketError, PeerLane};

/// The two lanes' queues and assemblers, each with its own budget.
#[derive(Debug, PartialEq, Eq)]
pub struct AttachmentPeerPacketLanes {
    control_outbound: AttachmentPacketQueue,
    control_inbound: AttachmentPacketAssembler,
    data_outbound: AttachmentPacketQueue,
    data_inbound: AttachmentPacketAssembler,
}

impl AttachmentPeerPacketLanes {
    /// Four empty lanes, each with its own budget.
    #[must_use]
    pub fn new() -> Self {
        Self {
            control_outbound: AttachmentPacketQueue::new(PeerLane::Control),
            control_inbound: AttachmentPacketAssembler::new(PeerLane::Control),
            data_outbound: AttachmentPacketQueue::new(PeerLane::Data),
            data_inbound: AttachmentPacketAssembler::new(PeerLane::Data),
        }
    }

    /// Whether any lane still holds an outbound message, which is what decides
    /// whether another flush pass is owed.
    #[must_use]
    pub fn has_queued_packets(&self) -> bool {
        ATTACHMENT_PEER_LANES
            .iter()
            .any(|definition| self.outbound(definition.lane).message_count() > 0)
    }

    /// The outbound FIFO for one lane.
    #[must_use]
    pub fn outbound(&self, lane: PeerLane) -> &AttachmentPacketQueue {
        match lane {
            PeerLane::Control => &self.control_outbound,
            PeerLane::Data => &self.data_outbound,
        }
    }

    /// The outbound FIFO for one lane, for a host that is flushing it.
    pub fn outbound_mut(&mut self, lane: PeerLane) -> &mut AttachmentPacketQueue {
        match lane {
            PeerLane::Control => &mut self.control_outbound,
            PeerLane::Data => &mut self.data_outbound,
        }
    }

    /// The inbound assembler for one lane.
    #[must_use]
    pub fn inbound(&self, lane: PeerLane) -> &AttachmentPacketAssembler {
        match lane {
            PeerLane::Control => &self.control_inbound,
            PeerLane::Data => &self.data_inbound,
        }
    }

    /// The inbound assembler for one lane, for the owner that receives on it.
    pub fn inbound_mut(&mut self, lane: PeerLane) -> &mut AttachmentPacketAssembler {
        match lane {
            PeerLane::Control => &mut self.control_inbound,
            PeerLane::Data => &mut self.data_inbound,
        }
    }

    /// Accept one inbound packet, returning a whole logical frame when the
    /// message completes.
    pub fn receive(
        &mut self,
        lane: PeerLane,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<Option<Vec<u8>>, AttachmentPacketError> {
        self.inbound_mut(lane).push(packet, now_ms)
    }

    /// Whether a lane is holding an unfinished message, which is what a host
    /// arms its stall deadline on.
    #[must_use]
    pub fn has_partial_message(&self, lane: PeerLane) -> bool {
        self.inbound(lane).has_partial_message()
    }

    /// Release a lane's stalled buffer if its window has passed. `true` means
    /// the peer is retired.
    pub fn expire(&mut self, lane: PeerLane, now_ms: u64) -> bool {
        self.inbound_mut(lane).expire(now_ms)
    }

    /// Release every retained buffer and close all four lanes.
    pub fn clear(&mut self) {
        for definition in ATTACHMENT_PEER_LANES {
            let lane = definition.lane;
            self.outbound_mut(lane).clear();
            self.inbound_mut(lane).reset();
        }
    }
}

impl Default for AttachmentPeerPacketLanes {
    fn default() -> Self {
        Self::new()
    }
}
