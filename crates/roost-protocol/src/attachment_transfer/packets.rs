//! The attachment-only outer framing on the two attachment data channels:
//! magic, version, message id, total bytes, offset, then payload. It shares no
//! magic, lane or state with the terminal peer's framing. Ports
//! `packages/protocol/src/attachment-transfer-packets.ts`; used by
//! `packet_queue` and the worker's attachment peer packet port.

use std::fmt;

use super::{
    PACKET_HEADER_BYTES, PACKET_LOGICAL_FRAME_MAX_BYTES, PACKET_MAGIC, PACKET_MAX_BYTES,
    PACKET_MAX_PAYLOAD_BYTES, PACKET_STALL_MS, PACKET_VERSION,
};

/// Retained-byte accounting is independent per direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttachmentTransferPacketDirection {
    Incoming,
    Outgoing,
}

/// Why a packet, fragment or queue operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttachmentTransferPacketError {
    PacketSize,
    PacketMagic,
    PacketVersion,
    PacketHeader,
    MessageId,
    MessageIdWrap,
    MessageSize,
    FragmentOrder,
    FragmentStalled,
    Quota,
    Allocation,
    Closed,
}

impl AttachmentTransferPacketError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PacketSize => "packet-size",
            Self::PacketMagic => "packet-magic",
            Self::PacketVersion => "packet-version",
            Self::PacketHeader => "packet-header",
            Self::MessageId => "message-id",
            Self::MessageIdWrap => "message-id-wrap",
            Self::MessageSize => "message-size",
            Self::FragmentOrder => "fragment-order",
            Self::FragmentStalled => "fragment-stalled",
            Self::Quota => "quota",
            Self::Allocation => "allocation",
            Self::Closed => "closed",
        }
    }
}

impl fmt::Display for AttachmentTransferPacketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "attachment transfer packet rejected: {}",
            self.as_str()
        )
    }
}

impl std::error::Error for AttachmentTransferPacketError {}

/// The identity and position of one fragment inside its logical message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentTransferPacketHeader {
    pub message_id: u32,
    pub total_bytes: u32,
    pub offset_bytes: u32,
}

/// One parsed packet; the payload borrows the caller's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentTransferPacket<'packet> {
    pub header: AttachmentTransferPacketHeader,
    pub payload: &'packet [u8],
}

/// Retained-byte accounting for one channel. A reservation happens before an
/// allocation and is released on every path out, including refusals.
pub trait AttachmentTransferPacketQuota {
    fn reserve(&mut self, direction: AttachmentTransferPacketDirection, bytes: usize) -> bool;
    fn release(&mut self, direction: AttachmentTransferPacketDirection, bytes: usize);
}

/// Encodes one complete packet. The header is validated first, so an encoded
/// packet always parses back.
pub fn encode_attachment_transfer_packet(
    header: AttachmentTransferPacketHeader,
    payload: &[u8],
) -> Result<Vec<u8>, AttachmentTransferPacketError> {
    assert_packet_fields(header, payload.len())?;
    let mut packet = Vec::new();
    packet
        .try_reserve_exact(PACKET_HEADER_BYTES + payload.len())
        .map_err(|_| AttachmentTransferPacketError::Allocation)?;
    for field in [
        PACKET_MAGIC,
        PACKET_VERSION,
        header.message_id,
        header.total_bytes,
        header.offset_bytes,
    ] {
        packet.extend_from_slice(&field.to_le_bytes());
    }
    packet.extend_from_slice(payload);
    Ok(packet)
}

/// Parses and bounds one outer packet without allocating a logical buffer.
pub fn parse_attachment_transfer_packet(
    packet: &[u8],
) -> Result<AttachmentTransferPacket<'_>, AttachmentTransferPacketError> {
    if packet.len() <= PACKET_HEADER_BYTES || packet.len() > PACKET_MAX_BYTES {
        return Err(AttachmentTransferPacketError::PacketSize);
    }
    if read_u32(packet, 0) != PACKET_MAGIC {
        return Err(AttachmentTransferPacketError::PacketMagic);
    }
    if read_u32(packet, 4) != PACKET_VERSION {
        return Err(AttachmentTransferPacketError::PacketVersion);
    }
    let header = AttachmentTransferPacketHeader {
        message_id: read_u32(packet, 8),
        total_bytes: read_u32(packet, 12),
        offset_bytes: read_u32(packet, 16),
    };
    let payload = &packet[PACKET_HEADER_BYTES..];
    assert_packet_fields(header, payload.len())?;
    Ok(AttachmentTransferPacket { header, payload })
}

struct PartialMessage {
    message_id: u32,
    total_bytes: u32,
    bytes: Vec<u8>,
    last_fragment_at_ms: u64,
}

/// One ordered attachment channel's single-message reassembly state.
///
/// Every timestamp is the caller's monotonic reading in milliseconds: this
/// crate reads no clock, and a stall window measured against one no test can
/// reach would be untestable.
pub struct AttachmentTransferPacketAssembler<Q: AttachmentTransferPacketQuota> {
    direction: AttachmentTransferPacketDirection,
    quota: Q,
    partial: Option<PartialMessage>,
    last_completed_message_id: u32,
    closed: bool,
}

impl<Q: AttachmentTransferPacketQuota> fmt::Debug for AttachmentTransferPacketAssembler<Q> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentTransferPacketAssembler")
            .field("direction", &self.direction)
            .field("last_message_id", &self.last_completed_message_id)
            .field("partial", &self.partial.is_some())
            .field("closed", &self.closed)
            .finish()
    }
}

impl<Q: AttachmentTransferPacketQuota> AttachmentTransferPacketAssembler<Q> {
    pub fn new(direction: AttachmentTransferPacketDirection, quota: Q) -> Self {
        Self {
            direction,
            quota,
            partial: None,
            last_completed_message_id: 0,
            closed: false,
        }
    }

    pub fn has_partial_message(&self) -> bool {
        self.partial.is_some()
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn last_message_id(&self) -> u32 {
        self.last_completed_message_id
    }

    /// Returns an owned logical frame once every ordered fragment arrived. Any
    /// refusal closes the assembler and releases its partial reservation.
    pub fn push(
        &mut self,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<Option<Vec<u8>>, AttachmentTransferPacketError> {
        if self.closed {
            return Err(AttachmentTransferPacketError::Closed);
        }
        let accepted = self.stalled(now_ms).and_then(|()| {
            let parsed = parse_attachment_transfer_packet(packet)?;
            self.accept(parsed, now_ms)
        });
        if accepted.is_err() {
            self.close_internal();
        }
        accepted
    }

    /// Releases a stalled partial buffer so the caller can retire the peer.
    pub fn expire(&mut self, now_ms: u64) -> bool {
        let Some(partial) = &self.partial else {
            return false;
        };
        if now_ms.saturating_sub(partial.last_fragment_at_ms) < PACKET_STALL_MS {
            return false;
        }
        self.close_internal();
        true
    }

    /// Releases retained state and starts a fresh ordered message sequence.
    pub fn reset(&mut self) {
        self.release_partial();
        self.last_completed_message_id = 0;
        self.closed = false;
    }

    fn stalled(&self, now_ms: u64) -> Result<(), AttachmentTransferPacketError> {
        match &self.partial {
            Some(partial)
                if now_ms.saturating_sub(partial.last_fragment_at_ms) >= PACKET_STALL_MS =>
            {
                Err(AttachmentTransferPacketError::FragmentStalled)
            }
            _ => Ok(()),
        }
    }

    fn accept(
        &mut self,
        packet: AttachmentTransferPacket<'_>,
        now_ms: u64,
    ) -> Result<Option<Vec<u8>>, AttachmentTransferPacketError> {
        let Some(partial) = &mut self.partial else {
            return self.begin(packet, now_ms);
        };
        let header = packet.header;
        if header.message_id != partial.message_id
            || header.total_bytes != partial.total_bytes
            || header.offset_bytes as usize != partial.bytes.len()
        {
            return Err(AttachmentTransferPacketError::FragmentOrder);
        }
        partial.bytes.extend_from_slice(packet.payload);
        partial.last_fragment_at_ms = now_ms;
        if partial.bytes.len() != partial.total_bytes as usize {
            return Ok(None);
        }
        Ok(Some(self.complete_partial()))
    }

    fn begin(
        &mut self,
        packet: AttachmentTransferPacket<'_>,
        now_ms: u64,
    ) -> Result<Option<Vec<u8>>, AttachmentTransferPacketError> {
        if self.last_completed_message_id == u32::MAX {
            return Err(AttachmentTransferPacketError::MessageIdWrap);
        }
        let header = packet.header;
        if header.message_id != self.last_completed_message_id + 1 || header.offset_bytes != 0 {
            return Err(AttachmentTransferPacketError::MessageId);
        }
        let total_bytes = header.total_bytes as usize;
        if !self.quota.reserve(self.direction, total_bytes) {
            return Err(AttachmentTransferPacketError::Quota);
        }
        if packet.payload.len() == total_bytes {
            // A single-packet message is copied out and released at once: it
            // is never retained, so it never holds the quota past this call.
            let bytes = packet.payload.to_vec();
            self.quota.release(self.direction, total_bytes);
            self.last_completed_message_id = header.message_id;
            return Ok(Some(bytes));
        }
        let mut bytes = Vec::new();
        if bytes.try_reserve_exact(total_bytes).is_err() {
            self.quota.release(self.direction, total_bytes);
            return Err(AttachmentTransferPacketError::Allocation);
        }
        bytes.extend_from_slice(packet.payload);
        self.partial = Some(PartialMessage {
            message_id: header.message_id,
            total_bytes: header.total_bytes,
            bytes,
            last_fragment_at_ms: now_ms,
        });
        Ok(None)
    }

    fn complete_partial(&mut self) -> Vec<u8> {
        let Some(partial) = self.partial.take() else {
            return Vec::new();
        };
        self.last_completed_message_id = partial.message_id;
        self.quota
            .release(self.direction, partial.total_bytes as usize);
        partial.bytes
    }

    fn release_partial(&mut self) {
        if let Some(partial) = self.partial.take() {
            self.quota
                .release(self.direction, partial.total_bytes as usize);
        }
    }

    fn close_internal(&mut self) {
        self.closed = true;
        self.release_partial();
    }
}

// The header invariants in the order a malformed header is reported: the id,
// then the logical size, then this fragment's fit inside it.
pub(super) fn assert_packet_fields(
    header: AttachmentTransferPacketHeader,
    payload_bytes: usize,
) -> Result<(), AttachmentTransferPacketError> {
    if header.message_id < 1 {
        return Err(AttachmentTransferPacketError::MessageId);
    }
    let total_bytes = header.total_bytes as usize;
    if !(1..=PACKET_LOGICAL_FRAME_MAX_BYTES).contains(&total_bytes) {
        return Err(AttachmentTransferPacketError::MessageSize);
    }
    let offset_bytes = header.offset_bytes as usize;
    if offset_bytes >= total_bytes
        || !(1..=PACKET_MAX_PAYLOAD_BYTES).contains(&payload_bytes)
        || offset_bytes + payload_bytes > total_bytes
    {
        return Err(AttachmentTransferPacketError::PacketHeader);
    }
    Ok(())
}

fn read_u32(packet: &[u8], offset: usize) -> u32 {
    let mut field = [0_u8; 4];
    field.copy_from_slice(&packet[offset..offset + 4]);
    u32::from_le_bytes(field)
}
