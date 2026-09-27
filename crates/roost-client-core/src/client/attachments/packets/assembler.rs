//! Single-message reassembly for one ordered attachment lane, inbound. At most
//! one partial message is held, message ids must be consecutive from 1, and one
//! rejection closes the generation. Ported from
//! `attachment-transfer-packets.ts`. Depends on the parent's parser and the
//! lane's byte budget, which it reserves before allocating.

use super::{
    ATTACHMENT_PACKET_MAX_PAYLOAD_BYTES, ATTACHMENT_PACKET_STALL_MS, AttachmentPacket,
    AttachmentPacketError, AttachmentPacketQuota, PeerLane, parse_attachment_packet,
};

#[derive(Debug, PartialEq, Eq)]
struct PartialMessage {
    message_id: u32,
    total_bytes: usize,
    bytes: Vec<u8>,
    next_offset: usize,
    last_fragment_at_ms: u64,
}

/// One lane's inbound reassembly state.
#[derive(Debug, PartialEq, Eq)]
pub struct AttachmentPacketAssembler {
    lane: PeerLane,
    quota: AttachmentPacketQuota,
    partial: Option<PartialMessage>,
    last_completed_message_id: u32,
    closed: bool,
}

impl AttachmentPacketAssembler {
    /// An empty assembler on `lane`, bounded by that lane's own budget.
    #[must_use]
    pub fn new(lane: PeerLane) -> Self {
        Self {
            lane,
            quota: AttachmentPacketQuota::for_lane(lane),
            partial: None,
            last_completed_message_id: 0,
            closed: false,
        }
    }

    /// The lane this assembler belongs to.
    #[must_use]
    pub fn lane(&self) -> PeerLane {
        self.lane
    }

    /// Whether a message is half-assembled, which is what a host arms its stall
    /// deadline on.
    #[must_use]
    pub fn has_partial_message(&self) -> bool {
        self.partial.is_some()
    }

    /// Whether this lane's generation is closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// The last message id this lane completed.
    #[must_use]
    pub fn last_message_id(&self) -> u32 {
        self.last_completed_message_id
    }

    /// Bytes this lane is holding for the current partial message.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.quota.retained_bytes()
    }

    /// Accept one packet and return a fully owned logical frame, or `None`
    /// until the message's final fragment arrives.
    ///
    /// `now_ms` is the caller's monotonic reading: the original's default of
    /// `performance.now()` went with the platform it came from, and a stall
    /// window measured against a clock this crate cannot see is one no test can
    /// reach.
    pub fn push(
        &mut self,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<Option<Vec<u8>>, AttachmentPacketError> {
        if self.closed {
            return Err(AttachmentPacketError::Closed);
        }
        match self.accept(packet, now_ms) {
            Ok(message) => Ok(message),
            Err(error) => {
                self.close();
                Err(error)
            }
        }
    }

    /// Release a stalled partial buffer. `true` means the peer is retired,
    /// because the lane has stopped making progress.
    pub fn expire(&mut self, now_ms: u64) -> bool {
        let Some(partial) = &self.partial else {
            return false;
        };
        if now_ms.saturating_sub(partial.last_fragment_at_ms) < ATTACHMENT_PACKET_STALL_MS {
            return false;
        }
        self.close();
        true
    }

    /// Releases retained state and opens a fresh ordered message sequence.
    pub fn reset(&mut self) {
        self.release_partial();
        self.last_completed_message_id = 0;
        self.closed = false;
    }

    fn accept(
        &mut self,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<Option<Vec<u8>>, AttachmentPacketError> {
        // The stall window is checked before the bytes are parsed: a peer that
        // went silent is retired on the fragment that arrives after the window,
        // not on whatever else that fragment happens to break.
        if let Some(partial) = &self.partial
            && now_ms.saturating_sub(partial.last_fragment_at_ms) >= ATTACHMENT_PACKET_STALL_MS
        {
            return Err(AttachmentPacketError::FragmentStalled);
        }
        let parsed = parse_attachment_packet(packet)?;
        if self.partial.is_none() {
            return self.begin(&parsed, now_ms);
        }
        let partial = self
            .partial
            .as_mut()
            .ok_or(AttachmentPacketError::FragmentOrder)?;
        if parsed.header.message_id != partial.message_id
            || parsed.header.total_bytes as usize != partial.total_bytes
            || parsed.header.offset_bytes as usize != partial.next_offset
        {
            return Err(AttachmentPacketError::FragmentOrder);
        }
        if parsed.payload.len() > ATTACHMENT_PACKET_MAX_PAYLOAD_BYTES {
            return Err(AttachmentPacketError::PacketHeader);
        }
        let end = partial.next_offset + parsed.payload.len();
        partial.bytes[partial.next_offset..end].copy_from_slice(parsed.payload);
        partial.next_offset = end;
        partial.last_fragment_at_ms = now_ms;
        if end != partial.total_bytes {
            return Ok(None);
        }
        self.complete_partial().map(Some)
    }

    fn begin(
        &mut self,
        packet: &AttachmentPacket<'_>,
        now_ms: u64,
    ) -> Result<Option<Vec<u8>>, AttachmentPacketError> {
        if self.last_completed_message_id == u32::MAX {
            return Err(AttachmentPacketError::MessageIdWrap);
        }
        if packet.header.message_id != self.last_completed_message_id + 1
            || packet.header.offset_bytes != 0
        {
            return Err(AttachmentPacketError::MessageId);
        }
        let total_bytes = packet.header.total_bytes as usize;
        if packet.payload.len() == total_bytes {
            return self.copy_whole_packet(packet, total_bytes);
        }
        if !self.quota.reserve(total_bytes) {
            return Err(AttachmentPacketError::Quota);
        }
        let mut bytes = vec![0u8; total_bytes];
        bytes[..packet.payload.len()].copy_from_slice(packet.payload);
        self.partial = Some(PartialMessage {
            message_id: packet.header.message_id,
            total_bytes,
            bytes,
            next_offset: packet.payload.len(),
            last_fragment_at_ms: now_ms,
        });
        Ok(None)
    }

    /// A frame that arrived whole is copied straight out, but it still has to
    /// FIT the lane: it makes the same reservation check a fragmented frame
    /// does, so a peer cannot pick the cheaper path to get past it.
    fn copy_whole_packet(
        &mut self,
        packet: &AttachmentPacket<'_>,
        total_bytes: usize,
    ) -> Result<Option<Vec<u8>>, AttachmentPacketError> {
        if !self.quota.reserve(total_bytes) {
            return Err(AttachmentPacketError::Quota);
        }
        self.last_completed_message_id = packet.header.message_id;
        self.quota.release(total_bytes);
        Ok(Some(packet.payload.to_vec()))
    }

    fn complete_partial(&mut self) -> Result<Vec<u8>, AttachmentPacketError> {
        let partial = self
            .partial
            .take()
            .ok_or(AttachmentPacketError::FragmentOrder)?;
        self.last_completed_message_id = partial.message_id;
        self.quota.release(partial.total_bytes);
        Ok(partial.bytes)
    }

    fn release_partial(&mut self) {
        if let Some(partial) = self.partial.take() {
            self.quota.release(partial.total_bytes);
        }
    }

    fn close(&mut self) {
        self.closed = true;
        self.release_partial();
    }
}
