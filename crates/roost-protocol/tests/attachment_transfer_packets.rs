//! Attachment peer framing, retained-byte ownership and order, ported from
//! `packages/protocol/tests/attachment-transfer-packets.test.ts` (and the bounds
//! case of `attachment-transfer.test.ts`). v2's "absent, non-finite, negative
//! clock" case has no Rust counterpart: `now_ms` is a `u64` the caller owns.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use roost_protocol::attachment_transfer::{
    AttachmentTransferPacketAssembler, AttachmentTransferPacketDirection as Direction,
    AttachmentTransferPacketError, AttachmentTransferPacketHeader, AttachmentTransferPacketQueue,
    AttachmentTransferPacketQuota, PACKET_HEADER_BYTES, PACKET_LOGICAL_FRAME_MAX_BYTES,
    PACKET_MAX_PAYLOAD_BYTES, PACKET_STALL_MS, PACKET_VERSION, PEER_DATA_CHANNELS, PeerChannelLane,
    encode_attachment_transfer_packet, is_chunk_sha256, parse_attachment_transfer_packet,
};

#[derive(Default)]
struct Recording {
    incoming: usize,
    outgoing: usize,
    releases: Vec<(Direction, usize)>,
}

/// One quota shared by an assembler and a queue, as the v2 test shares one.
#[derive(Clone)]
struct RecordingQuota {
    limit: usize,
    state: Rc<RefCell<Recording>>,
}

impl RecordingQuota {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            state: Rc::default(),
        }
    }

    fn reserved(&self) -> (usize, usize) {
        let state = self.state.borrow();
        (state.incoming, state.outgoing)
    }
}

impl AttachmentTransferPacketQuota for RecordingQuota {
    fn reserve(&mut self, direction: Direction, bytes: usize) -> bool {
        let mut state = self.state.borrow_mut();
        let slot = match direction {
            Direction::Incoming => &mut state.incoming,
            Direction::Outgoing => &mut state.outgoing,
        };
        if *slot + bytes > self.limit {
            return false;
        }
        *slot += bytes;
        true
    }

    fn release(&mut self, direction: Direction, bytes: usize) {
        let mut state = self.state.borrow_mut();
        state.releases.push((direction, bytes));
        match direction {
            Direction::Incoming => state.incoming -= bytes,
            Direction::Outgoing => state.outgoing -= bytes,
        }
    }
}

fn packet(message_id: u32, total_bytes: usize, offset_bytes: usize, payload: &[u8]) -> Vec<u8> {
    let header = AttachmentTransferPacketHeader {
        message_id,
        total_bytes: u32::try_from(total_bytes).unwrap(),
        offset_bytes: u32::try_from(offset_bytes).unwrap(),
    };
    encode_attachment_transfer_packet(header, payload).unwrap()
}

#[test]
fn writes_an_attachment_only_little_endian_magic_and_version_header() {
    let encoded = packet(0x0102_0304, 3, 0, &[9, 8, 7]);
    assert_eq!(
        &encoded[..PACKET_HEADER_BYTES],
        &[
            0x41, 0x54, 0x50, 0x31, 0x01, 0x00, 0x00, 0x00, 0x04, 0x03, 0x02, 0x01, 0x03, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]
    );
    let parsed = parse_attachment_transfer_packet(&encoded).unwrap();
    assert_eq!(parsed.header.message_id, 0x0102_0304);
    assert_eq!(parsed.header.total_bytes, 3);
    assert_eq!(parsed.header.offset_bytes, 0);
    assert_eq!(parsed.payload, &[9, 8, 7]);
}

#[test]
fn reassembles_only_consecutive_fragments_under_the_incoming_quota() {
    let quota = RecordingQuota::new(usize::MAX);
    let mut assembler = AttachmentTransferPacketAssembler::new(Direction::Incoming, quota.clone());
    let source: Vec<u8> = (0..PACKET_MAX_PAYLOAD_BYTES + 5)
        .map(|index| (index % 251) as u8)
        .collect();
    let first = packet(1, source.len(), 0, &source[..PACKET_MAX_PAYLOAD_BYTES]);
    assert_eq!(assembler.push(&first, 1).unwrap(), None);
    assert_eq!(quota.reserved().0, source.len());
    let rest = packet(
        1,
        source.len(),
        PACKET_MAX_PAYLOAD_BYTES,
        &source[PACKET_MAX_PAYLOAD_BYTES..],
    );
    assert_eq!(assembler.push(&rest, 2).unwrap(), Some(source.clone()));
    assert_eq!(quota.reserved().0, 0);
    assert_eq!(
        quota.state.borrow().releases,
        vec![(Direction::Incoming, source.len())]
    );
}

#[test]
fn rejects_terminal_magic_wrong_versions_malformed_order_and_frames_above_one_mib() {
    let mut terminal_magic = packet(1, 1, 0, &[1]);
    terminal_magic[..4].copy_from_slice(&0x3150_5452_u32.to_le_bytes());
    assert_eq!(
        parse_attachment_transfer_packet(&terminal_magic).unwrap_err(),
        AttachmentTransferPacketError::PacketMagic
    );
    let mut wrong_version = packet(1, 1, 0, &[1]);
    wrong_version[4..8].copy_from_slice(&(PACKET_VERSION + 1).to_le_bytes());
    assert_eq!(
        parse_attachment_transfer_packet(&wrong_version).unwrap_err(),
        AttachmentTransferPacketError::PacketVersion
    );
    let oversized = AttachmentTransferPacketHeader {
        message_id: 1,
        total_bytes: u32::try_from(PACKET_LOGICAL_FRAME_MAX_BYTES + 1).unwrap(),
        offset_bytes: 0,
    };
    assert_eq!(
        encode_attachment_transfer_packet(oversized, &[1]).unwrap_err(),
        AttachmentTransferPacketError::MessageSize
    );

    let mut assembler = AttachmentTransferPacketAssembler::new(
        Direction::Incoming,
        RecordingQuota::new(usize::MAX),
    );
    assert_eq!(assembler.push(&packet(1, 2, 0, &[1]), 1).unwrap(), None);
    assert_eq!(
        assembler.push(&packet(1, 2, 0, &[2]), 2).unwrap_err(),
        AttachmentTransferPacketError::FragmentOrder
    );
    assert!(assembler.is_closed());
}

#[test]
fn keeps_incoming_and_outgoing_reservations_separate_and_releases_a_stalled_partial() {
    let quota = RecordingQuota::new(PACKET_MAX_PAYLOAD_BYTES + 1);
    let mut incoming = AttachmentTransferPacketAssembler::new(Direction::Incoming, quota.clone());
    let mut outgoing = AttachmentTransferPacketQueue::new(Direction::Outgoing, quota.clone());
    let source = vec![0_u8; PACKET_MAX_PAYLOAD_BYTES + 1];
    let first = packet(1, source.len(), 0, &source[..PACKET_MAX_PAYLOAD_BYTES]);
    assert_eq!(incoming.push(&first, 100).unwrap(), None);
    assert!(outgoing.enqueue(source.clone()).unwrap());
    assert_eq!(quota.reserved(), (source.len(), source.len()));
    assert!(!incoming.expire(100 + PACKET_STALL_MS - 1));
    assert!(incoming.expire(100 + PACKET_STALL_MS));
    assert_eq!(quota.reserved(), (0, source.len()));
    outgoing.clear();
    assert_eq!(quota.reserved(), (0, 0));
}

#[test]
fn caches_each_fragment_until_accepted_once_commit_and_never_duplicates_a_buffered_send() {
    let quota = RecordingQuota::new(usize::MAX);
    let mut queue = AttachmentTransferPacketQueue::new(Direction::Outgoing, quota.clone());
    let source = vec![1_u8; PACKET_MAX_PAYLOAD_BYTES + 1];
    assert!(queue.enqueue(source.clone()).unwrap());

    let first_bytes = queue.next_fragment().unwrap().unwrap().bytes().to_vec();
    let again = queue.next_fragment().unwrap().unwrap();
    assert_eq!(again.bytes(), first_bytes.as_slice());
    assert_eq!(
        first_bytes.len(),
        PACKET_MAX_PAYLOAD_BYTES + PACKET_HEADER_BYTES
    );
    again.commit();

    let last = queue.next_fragment().unwrap().unwrap();
    let parsed = parse_attachment_transfer_packet(last.bytes()).unwrap();
    assert_eq!(parsed.header.message_id, 1);
    assert_eq!(parsed.header.total_bytes as usize, source.len());
    assert_eq!(
        parsed.header.offset_bytes as usize,
        PACKET_MAX_PAYLOAD_BYTES
    );
    assert!(last.final_fragment);
    last.commit();
    assert!(queue.next_fragment().unwrap().is_none());
    assert_eq!(quota.reserved().1, 0);
    assert_eq!(
        quota.state.borrow().releases,
        vec![(Direction::Outgoing, source.len())]
    );
}

#[test]
fn refuses_quota_admission_before_ownership_transfer_and_resets_after_close() {
    let quota = RecordingQuota::new(1);
    let mut queue = AttachmentTransferPacketQueue::new(Direction::Outgoing, quota.clone());
    assert!(!queue.enqueue(vec![1, 2]).unwrap());
    assert_eq!(queue.message_count(), 0);

    queue.clear();
    assert_eq!(
        queue.enqueue(vec![3]).unwrap_err(),
        AttachmentTransferPacketError::Closed
    );
    queue.reset();
    assert!(queue.enqueue(vec![4]).unwrap());
    let fragment = queue.next_fragment().unwrap().unwrap();
    assert_eq!(
        parse_attachment_transfer_packet(fragment.bytes())
            .unwrap()
            .header
            .message_id,
        1
    );
    fragment.commit();
    assert_eq!(quota.reserved().1, 0);
}

#[test]
fn centralizes_the_chunk_digest_shape_and_the_two_ordered_peer_channels() {
    let digest = "a".repeat(64);
    assert!(is_chunk_sha256(&digest));
    assert!(!is_chunk_sha256(&digest.to_uppercase()));
    assert!(!is_chunk_sha256(&"a".repeat(63)));
    let lanes: Vec<(PeerChannelLane, u16, &str, bool)> = PEER_DATA_CHANNELS
        .iter()
        .map(|channel| (channel.lane, channel.id, channel.label, channel.ordered))
        .collect();
    assert_eq!(
        lanes,
        vec![
            (
                PeerChannelLane::Control,
                0,
                "roost-attachment-control-v1",
                true
            ),
            (PeerChannelLane::Data, 1, "roost-attachment-data-v1", true),
        ]
    );
}
