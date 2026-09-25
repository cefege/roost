//! The 16-byte little-endian fragment header, its parser, and the assembler
//! that reassembles one logical message from its fragments.
//!
//! Every byte a browser receives for terminal data crosses this header, so the
//! layout is a wire contract, not an internal detail: magic, message id,
//! total logical bytes, this fragment's offset, then the payload. There is no
//! length field — the fragment is the rest of the packet, and completeness is
//! `offset + payload.len() <= total_bytes`.

use std::fmt;

pub use crate::terminal_peer::peer::{
    TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES, TERMINAL_PEER_PACKET_HEADER_BYTES,
    TERMINAL_PEER_PACKET_MAGIC, TERMINAL_PEER_PACKET_MAX_BYTES,
    TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES, TERMINAL_PEER_PACKET_STALL_MS,
    TerminalPeerLaneByteCaps, TerminalPeerPacketLane,
};

/// Why a packet was rejected, declared in the order `push` evaluates the
/// rules. That order is load-bearing: a fragment breaking several rules at
/// once reports the earliest, so the code a caller logs is the one that
/// stopped the message. `as_str` is the wire code both ends log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalPeerPacketErrorCode {
    Closed,
    FragmentStalled,
    MessageIdWrap,
    MessageId,
    MessageSize,
    PacketSize,
    PacketMagic,
    PacketHeader,
    FragmentOrder,
    Quota,
    Allocation,
}

impl TerminalPeerPacketErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::FragmentStalled => "fragment-stalled",
            Self::MessageIdWrap => "message-id-wrap",
            Self::MessageId => "message-id",
            Self::MessageSize => "message-size",
            Self::PacketSize => "packet-size",
            Self::PacketMagic => "packet-magic",
            Self::PacketHeader => "packet-header",
            Self::FragmentOrder => "fragment-order",
            Self::Quota => "quota",
            Self::Allocation => "allocation",
        }
    }
}

impl fmt::Display for TerminalPeerPacketErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.as_str();
        write!(formatter, "terminal peer packet rejected: {code}")
    }
}

impl std::error::Error for TerminalPeerPacketErrorCode {}

/// The name the carrier's callers use for a rejection. Every rejection fails
/// the channel generation closed: a peer that sent one malformed fragment is
/// no longer trusted with the bytes it already holds.
pub type TerminalPeerPacketError = TerminalPeerPacketErrorCode;

/// The three header fields without the magic: the identity and position of one
/// fragment inside the logical message it belongs to. Ids start at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalPeerPacketHeader {
    pub message_id: u32,
    pub total_bytes: u32,
    pub offset_bytes: u32,
}

/// One bounded packet. The payload borrows the transport's bytes; the
/// assembler copies before it retains anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalPeerPacket<'a> {
    pub header: TerminalPeerPacketHeader,
    pub payload: &'a [u8],
}

/// Retained-byte accounting for one direction and one peer. The assembler
/// reserves before it allocates and releases on every path out, so a peer
/// cannot make a receiver retain more than the budget it was given.
pub trait TerminalPeerPacketQuota {
    fn reserve(&mut self, bytes: usize) -> bool;
    fn release(&mut self, bytes: usize);
}

/// Encodes one complete packet, header included. The header is validated
/// first, so an encoded packet always parses back.
pub fn encode_terminal_peer_packet(
    lane: TerminalPeerPacketLane,
    header: TerminalPeerPacketHeader,
    payload: &[u8],
) -> Result<Vec<u8>, TerminalPeerPacketError> {
    assert_packet_fields(lane, header, payload.len())?;
    let mut packet = vec![0u8; TERMINAL_PEER_PACKET_HEADER_BYTES + payload.len()];
    packet[0..4].copy_from_slice(&TERMINAL_PEER_PACKET_MAGIC.to_le_bytes());
    packet[4..8].copy_from_slice(&header.message_id.to_le_bytes());
    packet[8..12].copy_from_slice(&header.total_bytes.to_le_bytes());
    packet[12..16].copy_from_slice(&header.offset_bytes.to_le_bytes());
    packet[TERMINAL_PEER_PACKET_HEADER_BYTES..].copy_from_slice(payload);
    Ok(packet)
}

/// Parses and bounds one raw packet, reserving and allocating nothing.
pub fn parse_terminal_peer_packet<'packet>(
    lane: TerminalPeerPacketLane,
    packet: &'packet [u8],
) -> Result<TerminalPeerPacket<'packet>, TerminalPeerPacketError> {
    // With no length field, a header with no payload is indistinguishable
    // from a truncated one, so both are refused here.
    if !(TERMINAL_PEER_PACKET_HEADER_BYTES + 1..=TERMINAL_PEER_PACKET_MAX_BYTES)
        .contains(&packet.len())
    {
        return Err(TerminalPeerPacketError::PacketSize);
    }
    if read_u32(packet, 0) != TERMINAL_PEER_PACKET_MAGIC {
        return Err(TerminalPeerPacketError::PacketMagic);
    }
    let header = TerminalPeerPacketHeader {
        message_id: read_u32(packet, 4),
        total_bytes: read_u32(packet, 8),
        offset_bytes: read_u32(packet, 12),
    };
    let payload = &packet[TERMINAL_PEER_PACKET_HEADER_BYTES..];
    assert_packet_fields(lane, header, payload.len())?;
    Ok(TerminalPeerPacket { header, payload })
}

fn read_u32(packet: &[u8], offset: usize) -> u32 {
    let bytes = &packet[offset..offset + 4];
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

// The header invariants, in the order a malformed header is reported: the id,
// then the logical size against its lane, then the fragment's fit.
fn assert_packet_fields(
    lane: TerminalPeerPacketLane,
    header: TerminalPeerPacketHeader,
    payload_bytes: usize,
) -> Result<(), TerminalPeerPacketError> {
    if header.message_id < 1 {
        return Err(TerminalPeerPacketError::MessageId);
    }
    let total_bytes = header.total_bytes as usize;
    if total_bytes < 1 || total_bytes > TerminalPeerLaneByteCaps::for_lane(lane) {
        return Err(TerminalPeerPacketError::MessageSize);
    }
    let offset_bytes = header.offset_bytes as usize;
    if offset_bytes >= total_bytes
        || !(1..=TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES).contains(&payload_bytes)
        || offset_bytes + payload_bytes > total_bytes
    {
        return Err(TerminalPeerPacketError::PacketHeader);
    }
    Ok(())
}

struct PartialMessage {
    message_id: u32,
    total_bytes: usize,
    bytes: Vec<u8>,
    next_offset: usize,
    last_fragment_at_ms: u64,
}

/// One ordered channel's consecutive single-message reassembly state.
///
/// It holds at most one partial message, because the direct path is ordered: a
/// fragment that does not continue the current message exactly belongs to a
/// peer that has lost the sequence, not to one that is merely late.
pub struct TerminalPeerPacketAssembler<Q: TerminalPeerPacketQuota> {
    lane: TerminalPeerPacketLane,
    quota: Q,
    partial: Option<PartialMessage>,
    last_completed_message_id: u32,
    closed: bool,
}

impl<Q: TerminalPeerPacketQuota> fmt::Debug for TerminalPeerPacketAssembler<Q> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = (self.lane, self.last_completed_message_id, self.closed);
        write!(formatter, "TerminalPeerPacketAssembler({state:?})")
    }
}

impl<Q: TerminalPeerPacketQuota> TerminalPeerPacketAssembler<Q> {
    pub fn new(lane: TerminalPeerPacketLane, quota: Q) -> Self {
        Self {
            lane,
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

    /// Accepts one fragment and returns a fully owned logical payload, or
    /// `None` until the message's final fragment arrives.
    ///
    /// `now_ms` is the caller's monotonic reading: the original's default of
    /// `performance.now()` went with the platform it came from, and a stall
    /// window measured against a clock this crate cannot see is one no test
    /// can reach.
    pub fn push(
        &mut self,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<Option<Vec<u8>>, TerminalPeerPacketError> {
        if self.closed {
            return Err(TerminalPeerPacketError::Closed);
        }
        match self.accept(packet, now_ms) {
            Ok(message) => Ok(message),
            Err(error) => {
                self.close();
                Err(error)
            }
        }
    }

    /// Releases a stalled partial buffer; the caller retires the peer when this
    /// is true, because the lane has stopped making progress.
    pub fn expire(&mut self, now_ms: u64) -> bool {
        let Some(partial) = &self.partial else {
            return false;
        };
        if now_ms.saturating_sub(partial.last_fragment_at_ms) < TERMINAL_PEER_PACKET_STALL_MS {
            return false;
        }
        self.close();
        true
    }

    pub fn reset(&mut self) {
        self.release_partial();
        self.last_completed_message_id = 0;
        self.closed = false;
    }

    fn accept(
        &mut self,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<Option<Vec<u8>>, TerminalPeerPacketError> {
        // The stall window is checked before the bytes are parsed: a peer that
        // went silent is retired on the fragment that arrives after the window,
        // not on whatever else that fragment happens to break.
        if let Some(partial) = &self.partial
            && now_ms.saturating_sub(partial.last_fragment_at_ms) >= TERMINAL_PEER_PACKET_STALL_MS
        {
            return Err(TerminalPeerPacketError::FragmentStalled);
        }
        let parsed = parse_terminal_peer_packet(self.lane, packet)?;
        if self.partial.is_none() {
            return self.begin(&parsed, now_ms);
        }
        let partial = self
            .partial
            .as_mut()
            .ok_or(TerminalPeerPacketError::FragmentOrder)?;
        if parsed.header.message_id != partial.message_id
            || parsed.header.total_bytes as usize != partial.total_bytes
            || parsed.header.offset_bytes as usize != partial.next_offset
        {
            return Err(TerminalPeerPacketError::FragmentOrder);
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
        packet: &TerminalPeerPacket<'_>,
        now_ms: u64,
    ) -> Result<Option<Vec<u8>>, TerminalPeerPacketError> {
        if self.last_completed_message_id == u32::MAX {
            return Err(TerminalPeerPacketError::MessageIdWrap);
        }
        if packet.header.message_id != self.last_completed_message_id + 1
            || packet.header.offset_bytes != 0
        {
            return Err(TerminalPeerPacketError::MessageId);
        }
        let total_bytes = packet.header.total_bytes as usize;
        if packet.payload.len() == total_bytes {
            return self.copy_single_packet(packet).map(Some);
        }
        // Reserved before the buffer exists and released again if the buffer
        // cannot be had, so a refused allocation never leaves the peer charged
        // for bytes it did not retain.
        if !self.quota.reserve(total_bytes) {
            return Err(TerminalPeerPacketError::Quota);
        }
        let bytes = match allocate_logical(total_bytes, packet.payload) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.quota.release(total_bytes);
                return Err(error);
            }
        };
        self.partial = Some(PartialMessage {
            message_id: packet.header.message_id,
            total_bytes,
            bytes,
            next_offset: packet.payload.len(),
            last_fragment_at_ms: now_ms,
        });
        Ok(None)
    }

    fn copy_single_packet(
        &mut self,
        packet: &TerminalPeerPacket<'_>,
    ) -> Result<Vec<u8>, TerminalPeerPacketError> {
        let total_bytes = packet.header.total_bytes as usize;
        if !self.quota.reserve(total_bytes) {
            return Err(TerminalPeerPacketError::Quota);
        }
        let bytes = allocate_logical(total_bytes, packet.payload);
        // The reservation covers the copy, not the buffer handed back.
        self.quota.release(total_bytes);
        let bytes = bytes?;
        self.last_completed_message_id = packet.header.message_id;
        Ok(bytes)
    }

    fn complete_partial(&mut self) -> Result<Vec<u8>, TerminalPeerPacketError> {
        let partial = self
            .partial
            .take()
            .ok_or(TerminalPeerPacketError::FragmentOrder)?;
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

/// One logical buffer, reserved rather than aborted on: a peer that names an
/// impossible size loses its channel, not the process. The caller owns the
/// retention reservation and releases it on the refusal path, so exactly one
/// release happens per reserve.
fn allocate_logical(
    total_bytes: usize,
    first_fragment: &[u8],
) -> Result<Vec<u8>, TerminalPeerPacketError> {
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(total_bytes).is_err() {
        return Err(TerminalPeerPacketError::Allocation);
    }
    bytes.resize(total_bytes, 0);
    bytes[..first_fragment.len()].copy_from_slice(first_fragment);
    Ok(bytes)
}

// The two states a peer cannot reach by sending — a wrapped id sequence and an
// impossible buffer — are pinned in a sibling module, so this file stays inside
// the size cap without losing the private access those tests need.
#[cfg(test)]
mod tests;
