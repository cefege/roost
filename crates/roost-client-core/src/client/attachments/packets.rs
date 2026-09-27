//! The attachment-only outer framing on the peer route's two ordered channels.
//! Magic, version, message id, total bytes, offset — a wire contract that shares
//! nothing with the terminal peer's lane. Ported from
//! `attachment-transfer-packets.ts`; the queue is in `packets::queue`, the
//! reassembly in `packets::assembler`, the lane owner in `packets::lanes`.

use std::fmt;

pub mod assembler;
pub mod lanes;
pub mod queue;

/// The magic that opens every attachment packet. `'A' 'T' 'P' '1'`, little
/// endian, and deliberately not the terminal peer's.
pub const ATTACHMENT_PACKET_MAGIC: u32 = 0x3150_5441;

/// The framing version. A packet that does not carry it is refused rather than
/// guessed at, because a mis-framed upload is a corrupt file on the far end.
pub const ATTACHMENT_PACKET_VERSION: u32 = 1;

/// magic, version, message id, total bytes, offset.
pub const ATTACHMENT_PACKET_HEADER_BYTES: usize = 20;

/// The largest one packet, header included. This is what the data channel's
/// message size is negotiated above, so exceeding it is a transport failure
/// rather than a slow send.
pub const ATTACHMENT_PACKET_MAX_BYTES: usize = 16_384;

/// The largest payload one packet may carry.
pub const ATTACHMENT_PACKET_MAX_PAYLOAD_BYTES: usize =
    ATTACHMENT_PACKET_MAX_BYTES - ATTACHMENT_PACKET_HEADER_BYTES;

/// The largest logical frame the lanes will assemble or queue. A frame larger
/// than this is refused before anything is reserved, so a peer cannot make a
/// receiver allocate on its say-so alone.
pub const ATTACHMENT_LOGICAL_FRAME_MAX_BYTES: usize = 1024 * 1024;

/// How long a partial message may go without a fragment before the lane is
/// retired. Long enough that one slow chunk is not a stall.
pub const ATTACHMENT_PACKET_STALL_MS: u64 = 10_000;

/// The control lane's budget, per direction.
pub const ATTACHMENT_CONTROL_QUEUE_MAX_BYTES: usize = 128 * 1024;

/// The data lane's budget, per direction. Large enough for a whole chunk's
/// worth of fragments queued while the channel drains.
pub const ATTACHMENT_DATA_QUEUE_MAX_BYTES: usize = 1024 * 1024;

/// The subprotocol both lanes negotiate. One string, because the worker's peer
/// accepts exactly this and nothing else.
pub const ATTACHMENT_PEER_DATA_CHANNEL_PROTOCOL: &str = "roost.attachment-transfer.v1";

/// The two ordered lanes, in creation order. The ids are fixed rather than
/// allocated, because both ends create the channels themselves and a
/// negotiated id would let the far end choose the lane this client believes is
/// control.
pub const ATTACHMENT_PEER_LANES: [AttachmentPeerLane; 2] = [
    AttachmentPeerLane {
        lane: PeerLane::Control,
        id: 0,
        label: "roost-attachment-control-v1",
    },
    AttachmentPeerLane {
        lane: PeerLane::Data,
        id: 1,
        label: "roost-attachment-data-v1",
    },
];

/// Which of the two channels a packet belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PeerLane {
    /// Authentication, acknowledgements and status. Never carries file bytes.
    Control,
    /// The chunk stream, and nothing else.
    Data,
}

impl PeerLane {
    /// The budget this lane's queues and assemblers are bounded by.
    #[must_use]
    pub fn queue_max_bytes(self) -> usize {
        match self {
            Self::Control => ATTACHMENT_CONTROL_QUEUE_MAX_BYTES,
            Self::Data => ATTACHMENT_DATA_QUEUE_MAX_BYTES,
        }
    }
}

/// One lane's static channel definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentPeerLane {
    pub lane: PeerLane,
    /// The pre-negotiated channel id.
    pub id: u16,
    /// The channel label, which is part of the contract with the worker.
    pub label: &'static str,
}

/// Why a packet was rejected, in the order the rules are evaluated.
///
/// The order is load-bearing: a fragment breaking several rules at once
/// reports the earliest, so the code a caller logs is the one that stopped the
/// message. `as_str` is the code both ends log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttachmentPacketError {
    /// The lane's generation is closed; nothing more is accepted on it.
    Closed,
    /// A fragment that continues a message stopped arriving.
    FragmentStalled,
    /// The message-id sequence has been exhausted, and a new generation is the
    /// only way to continue.
    MessageIdWrap,
    /// The message id is out of range, or is not the one the lane expects.
    MessageId,
    /// The logical frame is empty, or larger than the lane will assemble.
    MessageSize,
    /// The packet is empty of payload, or larger than one packet may be.
    PacketSize,
    /// The magic is not the attachment magic.
    PacketMagic,
    /// The version is not the framing version.
    PacketVersion,
    /// The header's own fields are inconsistent with this fragment.
    PacketHeader,
    /// A fragment that does not continue the current message exactly.
    FragmentOrder,
    /// The lane's byte budget is spent, so the caller must wait for it to drain.
    Quota,
    /// The queue could not grow, so nothing was retained and nothing is owed.
    Allocation,
    /// The clock this crate was handed is not a clock.
    Clock,
}

impl AttachmentPacketError {
    /// The wire code both ends log.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::FragmentStalled => "fragment-stalled",
            Self::MessageIdWrap => "message-id-wrap",
            Self::MessageId => "message-id",
            Self::MessageSize => "message-size",
            Self::PacketSize => "packet-size",
            Self::PacketMagic => "packet-magic",
            Self::PacketVersion => "packet-version",
            Self::PacketHeader => "packet-header",
            Self::FragmentOrder => "fragment-order",
            Self::Quota => "quota",
            Self::Allocation => "allocation",
            Self::Clock => "clock",
        }
    }
}

impl fmt::Display for AttachmentPacketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "attachment transfer packet rejected: {}",
            self.as_str()
        )
    }
}

impl std::error::Error for AttachmentPacketError {}

/// The three header fields without the magic: the identity and position of one
/// fragment inside the logical message it belongs to. Ids start at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentPacketHeader {
    pub message_id: u32,
    pub total_bytes: u32,
    pub offset_bytes: u32,
}

/// One bounded packet. The payload borrows the caller's bytes; everything that
/// retains anything copies first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentPacket<'a> {
    pub header: AttachmentPacketHeader,
    pub payload: &'a [u8],
}

/// Retained-byte accounting for one lane in one direction.
///
/// A concrete budget rather than a trait, because there is exactly one bound to
/// enforce and a trait here would be an invitation to enforce a different one.
/// Reservation happens before an allocation and release happens on every path
/// out, including the ones that refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentPacketQuota {
    maximum_bytes: usize,
    retained_bytes: usize,
}

impl AttachmentPacketQuota {
    /// A budget of `maximum_bytes` retained bytes, starting empty.
    #[must_use]
    pub fn new(maximum_bytes: usize) -> Self {
        Self {
            maximum_bytes,
            retained_bytes: 0,
        }
    }

    /// The lane's own budget.
    #[must_use]
    pub fn for_lane(lane: PeerLane) -> Self {
        Self::new(lane.queue_max_bytes())
    }

    /// Bytes this budget is currently holding.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// Whether `bytes` more would fit. The reservation itself is the caller's,
    /// because a caller that cannot go on to retain what it reserved must be
    /// able to give the reservation back.
    #[must_use]
    pub fn would_fit(&self, bytes: usize) -> bool {
        self.retained_bytes.saturating_add(bytes) <= self.maximum_bytes
    }

    /// Take `bytes` of the budget, or refuse.
    pub fn reserve(&mut self, bytes: usize) -> bool {
        if !self.would_fit(bytes) {
            return false;
        }
        self.retained_bytes += bytes;
        true
    }

    /// Give `bytes` back. Saturating, because a double release must not
    /// manufacture budget.
    pub fn release(&mut self, bytes: usize) {
        self.retained_bytes = self.retained_bytes.saturating_sub(bytes);
    }
}

/// Encode one complete packet, header included.
///
/// The header is validated first, so an encoded packet always parses back.
pub fn encode_attachment_packet(
    header: AttachmentPacketHeader,
    payload: &[u8],
) -> Result<Vec<u8>, AttachmentPacketError> {
    assert_packet_fields(header, payload.len())?;
    let mut packet = vec![0u8; ATTACHMENT_PACKET_HEADER_BYTES + payload.len()];
    write_u32(&mut packet, 0, ATTACHMENT_PACKET_MAGIC);
    write_u32(&mut packet, 4, ATTACHMENT_PACKET_VERSION);
    write_u32(&mut packet, 8, header.message_id);
    write_u32(&mut packet, 12, header.total_bytes);
    write_u32(&mut packet, 16, header.offset_bytes);
    packet[ATTACHMENT_PACKET_HEADER_BYTES..].copy_from_slice(payload);
    Ok(packet)
}

/// Parse and bounds one raw packet, reserving and allocating nothing.
pub fn parse_attachment_packet<'packet>(
    packet: &'packet [u8],
) -> Result<AttachmentPacket<'packet>, AttachmentPacketError> {
    // With no length field, a header with no payload is indistinguishable
    // from a truncated one, so both are refused here.
    if !(ATTACHMENT_PACKET_HEADER_BYTES + 1..=ATTACHMENT_PACKET_MAX_BYTES).contains(&packet.len()) {
        return Err(AttachmentPacketError::PacketSize);
    }
    if read_u32(packet, 0) != ATTACHMENT_PACKET_MAGIC {
        return Err(AttachmentPacketError::PacketMagic);
    }
    if read_u32(packet, 4) != ATTACHMENT_PACKET_VERSION {
        return Err(AttachmentPacketError::PacketVersion);
    }
    let header = AttachmentPacketHeader {
        message_id: read_u32(packet, 8),
        total_bytes: read_u32(packet, 12),
        offset_bytes: read_u32(packet, 16),
    };
    let payload = &packet[ATTACHMENT_PACKET_HEADER_BYTES..];
    assert_packet_fields(header, payload.len())?;
    Ok(AttachmentPacket { header, payload })
}

// The header invariants, in the order a malformed header is reported: the id,
// then the logical size against the lane's cap, then this fragment's fit.
fn assert_packet_fields(
    header: AttachmentPacketHeader,
    payload_bytes: usize,
) -> Result<(), AttachmentPacketError> {
    if header.message_id < 1 {
        return Err(AttachmentPacketError::MessageId);
    }
    let total_bytes = header.total_bytes as usize;
    if total_bytes < 1 || total_bytes > ATTACHMENT_LOGICAL_FRAME_MAX_BYTES {
        return Err(AttachmentPacketError::MessageSize);
    }
    let offset_bytes = header.offset_bytes as usize;
    if offset_bytes >= total_bytes
        || !(1..=ATTACHMENT_PACKET_MAX_PAYLOAD_BYTES).contains(&payload_bytes)
        || offset_bytes + payload_bytes > total_bytes
    {
        return Err(AttachmentPacketError::PacketHeader);
    }
    Ok(())
}

fn write_u32(packet: &mut [u8], offset: usize, value: u32) {
    packet[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn read_u32(packet: &[u8], offset: usize) -> u32 {
    let bytes = &packet[offset..offset + 4];
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
