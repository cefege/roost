//! The outer fragment framing, and the order the assembler rejects in.
//!
//! The 16-byte header is a wire contract with a real browser on the other end,
//! so the tests pin the exact bytes, the exact bounds, and the exact code a
//! fragment is refused with — including which of several broken rules is
//! reported when one fragment breaks all of them at once.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use roost_protocol::terminal_peer::packets::{
    TERMINAL_PEER_PACKET_HEADER_BYTES, TERMINAL_PEER_PACKET_MAGIC, TERMINAL_PEER_PACKET_MAX_BYTES,
    TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES, TerminalPeerPacketAssembler, TerminalPeerPacketError,
    TerminalPeerPacketHeader, TerminalPeerPacketQuota, encode_terminal_peer_packet,
    parse_terminal_peer_packet,
};
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_PACKET_STALL_MS, TerminalPeerLaneByteCaps, TerminalPeerPacketLane,
};

const LANE: TerminalPeerPacketLane = TerminalPeerPacketLane::Control;

#[derive(Default)]
struct RetainedBytes {
    held: usize,
    reserved: Vec<usize>,
    released: Vec<usize>,
}

/// The assembler owns its quota, so the ledger the test reads is shared rather
/// than borrowed back out.
struct RecordingQuota {
    limit: usize,
    ledger: Rc<RefCell<RetainedBytes>>,
}

impl TerminalPeerPacketQuota for RecordingQuota {
    fn reserve(&mut self, bytes: usize) -> bool {
        let mut ledger = self.ledger.borrow_mut();
        ledger.reserved.push(bytes);
        if ledger.held + bytes > self.limit {
            return false;
        }
        ledger.held += bytes;
        true
    }

    fn release(&mut self, bytes: usize) {
        let mut ledger = self.ledger.borrow_mut();
        ledger.released.push(bytes);
        ledger.held -= bytes;
    }
}

type Ledger = Rc<RefCell<RetainedBytes>>;

fn assembler(limit: usize) -> (TerminalPeerPacketAssembler<RecordingQuota>, Ledger) {
    let ledger: Ledger = Rc::new(RefCell::new(RetainedBytes::default()));
    let quota = RecordingQuota {
        limit,
        ledger: Rc::clone(&ledger),
    };
    (TerminalPeerPacketAssembler::new(LANE, quota), ledger)
}

fn header(message_id: u32, total_bytes: u32, offset_bytes: u32) -> TerminalPeerPacketHeader {
    TerminalPeerPacketHeader {
        message_id,
        total_bytes,
        offset_bytes,
    }
}

fn packet(message_id: u32, total_bytes: u32, offset_bytes: u32, payload: &[u8]) -> Vec<u8> {
    encode_terminal_peer_packet(LANE, header(message_id, total_bytes, offset_bytes), payload)
        .expect("a well-formed fragment encodes")
}

/// The same packet built byte by byte, for the shapes `encode` refuses.
fn raw_packet(message_id: u32, total_bytes: u32, offset_bytes: u32, payload: &[u8]) -> Vec<u8> {
    let mut packet = Vec::new();
    packet.extend_from_slice(&TERMINAL_PEER_PACKET_MAGIC.to_le_bytes());
    packet.extend_from_slice(&message_id.to_le_bytes());
    packet.extend_from_slice(&total_bytes.to_le_bytes());
    packet.extend_from_slice(&offset_bytes.to_le_bytes());
    packet.extend_from_slice(payload);
    packet
}

fn source_bytes(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index % 251) as u8).collect()
}

#[test]
fn the_header_is_the_mandatory_little_endian_layout() {
    let encoded = packet(0x0102_0304, 3, 0, &[9, 8, 7]);
    assert_eq!(
        &encoded[..TERMINAL_PEER_PACKET_HEADER_BYTES],
        &[
            0x52, 0x54, 0x50, 0x31, // ASCII "RTP1"
            0x04, 0x03, 0x02, 0x01, // message id
            0x03, 0x00, 0x00, 0x00, // total logical bytes
            0x00, 0x00, 0x00, 0x00, // this fragment's offset
        ]
    );
    let parsed = parse_terminal_peer_packet(LANE, &encoded).expect("an encoded packet parses");
    assert_eq!(parsed.header.message_id, 0x0102_0304);
    assert_eq!(parsed.header.total_bytes, 3);
    assert_eq!(parsed.header.offset_bytes, 0);
    assert_eq!(parsed.payload, &[9, 8, 7]);
}

#[test]
fn a_maximum_length_payload_round_trips_at_the_packet_cap() {
    let payload = source_bytes(TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES);
    let encoded = packet(1, payload.len() as u32, 0, &payload);
    assert_eq!(encoded.len(), TERMINAL_PEER_PACKET_MAX_BYTES);
    let parsed = parse_terminal_peer_packet(LANE, &encoded).expect("a full packet parses");
    assert_eq!(parsed.payload, payload.as_slice());
}

#[test]
fn a_zero_length_payload_is_refused_as_a_packet_with_no_fragment() {
    let error = encode_terminal_peer_packet(LANE, header(1, 1, 0), &[])
        .expect_err("a header with no payload is not a packet");
    assert_eq!(error, TerminalPeerPacketError::PacketHeader);
    let error = parse_terminal_peer_packet(LANE, &[0u8; TERMINAL_PEER_PACKET_HEADER_BYTES])
        .expect_err("a bare header is not a packet");
    assert_eq!(error, TerminalPeerPacketError::PacketSize);
}

#[test]
fn a_fragment_that_would_overflow_its_logical_message_is_refused() {
    let error = encode_terminal_peer_packet(LANE, header(1, 2, 1), &[9, 8, 7, 6])
        .expect_err("offset plus payload must not pass total");
    assert_eq!(error, TerminalPeerPacketError::PacketHeader);
    let error = parse_terminal_peer_packet(LANE, &raw_packet(1, 2, 1, &[9, 8, 7, 6]))
        .expect_err("a received fragment is bounded the same way");
    assert_eq!(error, TerminalPeerPacketError::PacketHeader);
}

#[test]
fn a_message_over_its_lane_cap_is_a_message_size_rejection() {
    let over = TerminalPeerLaneByteCaps::for_lane(LANE) as u32 + 1;
    let error = parse_terminal_peer_packet(LANE, &raw_packet(1, over, 0, &[1]))
        .expect_err("the control lane cannot carry a history-sized message");
    assert_eq!(error, TerminalPeerPacketError::MessageSize);
    let error = parse_terminal_peer_packet(LANE, &raw_packet(1, 0, 0, &[1]))
        .expect_err("a zero-byte logical message is refused");
    assert_eq!(error, TerminalPeerPacketError::MessageSize);
}

#[test]
fn every_rejection_code_is_the_string_the_browser_logs() {
    let codes = [
        (TerminalPeerPacketError::Closed, "closed"),
        (TerminalPeerPacketError::FragmentStalled, "fragment-stalled"),
        (TerminalPeerPacketError::MessageIdWrap, "message-id-wrap"),
        (TerminalPeerPacketError::MessageId, "message-id"),
        (TerminalPeerPacketError::MessageSize, "message-size"),
        (TerminalPeerPacketError::PacketSize, "packet-size"),
        (TerminalPeerPacketError::PacketMagic, "packet-magic"),
        (TerminalPeerPacketError::PacketHeader, "packet-header"),
        (TerminalPeerPacketError::FragmentOrder, "fragment-order"),
        (TerminalPeerPacketError::Quota, "quota"),
        (TerminalPeerPacketError::Allocation, "allocation"),
    ];
    for (code, wire) in codes {
        assert_eq!(code.as_str(), wire);
    }
}

#[test]
fn closed_outranks_every_other_rule() {
    let (mut state, _ledger) = assembler(usize::MAX);
    state
        .push(&packet(2, 1, 0, &[1]), 0)
        .expect_err("id 2 does not follow id 1");
    assert!(state.is_closed());
    let error = state
        .push(&packet(2, 1, 0, &[1]), 0)
        .expect_err("a closed generation refuses everything");
    assert_eq!(error, TerminalPeerPacketError::Closed);
    assert_eq!(state.last_message_id(), 0);
}

#[test]
fn a_stalled_partial_outranks_the_late_fragment_that_arrives() {
    let (mut state, ledger) = assembler(usize::MAX);
    state
        .push(&packet(1, 2, 0, &[1]), 100)
        .expect("the first fragment is retained");
    let error = state
        .push(
            &[0u8; TERMINAL_PEER_PACKET_HEADER_BYTES],
            100 + TERMINAL_PEER_PACKET_STALL_MS,
        )
        .expect_err("the stall window retires the lane");
    assert_eq!(error, TerminalPeerPacketError::FragmentStalled);
    // The partial is released on the way out, once.
    assert_eq!(ledger.borrow().released, vec![2]);
    assert_eq!(ledger.borrow().held, 0);
}

#[test]
fn a_packet_s_own_bounds_outrank_the_fragment_order() {
    let (mut state, _ledger) = assembler(usize::MAX);
    state
        .push(&packet(1, 2, 0, &[1]), 0)
        .expect("the first fragment is retained");
    let error = state
        .push(&[0u8; TERMINAL_PEER_PACKET_HEADER_BYTES], 0)
        .expect_err("a bare header is refused before the order is judged");
    assert_eq!(error, TerminalPeerPacketError::PacketSize);

    let (mut state, _ledger) = assembler(usize::MAX);
    state
        .push(&packet(1, 2, 0, &[1]), 0)
        .expect("the first fragment is retained");
    let error = state
        .push(&raw_packet(9, 1, 0, &[1]), 0)
        .expect_err("another message's fragment is not a continuation");
    assert_eq!(error, TerminalPeerPacketError::FragmentOrder);

    let (mut state, _ledger) = assembler(usize::MAX);
    let mut foreign_magic = raw_packet(1, 1, 0, &[1]);
    foreign_magic[0] ^= 0xff;
    let error = state
        .push(&foreign_magic, 0)
        .expect_err("the magic is read before the id sequence");
    assert_eq!(error, TerminalPeerPacketError::PacketMagic);
}

#[test]
fn the_header_rules_keep_their_own_order() {
    let (mut state, _ledger) = assembler(usize::MAX);
    let error = state
        .push(&raw_packet(0, 0, 0, &[1]), 0)
        .expect_err("the id is judged before the message size");
    assert_eq!(error, TerminalPeerPacketError::MessageId);

    // The zero-length message also fails the fragment fit, and the size rule
    // is the earlier of the two.
    let (mut state, _ledger) = assembler(usize::MAX);
    let error = state
        .push(&raw_packet(1, 0, 0, &[1]), 0)
        .expect_err("the message size is judged before the fragment fit");
    assert_eq!(error, TerminalPeerPacketError::MessageSize);
}

#[test]
fn a_two_fragment_message_reassembles_and_releases_once() {
    let source = source_bytes(TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES + 5);
    let total = source.len() as u32;
    let (mut state, ledger) = assembler(usize::MAX);
    let first = packet(
        1,
        total,
        0,
        &source[..TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES],
    );
    assert!(
        state
            .push(&first, 1)
            .expect("the first fragment is retained")
            .is_none()
    );
    assert!(state.has_partial_message());
    let last = packet(
        1,
        total,
        TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES as u32,
        &source[TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES..],
    );
    let completed = state
        .push(&last, 2)
        .expect("the final fragment completes the message")
        .expect("a completed message is returned");
    assert_eq!(completed, source);
    assert_eq!(state.last_message_id(), 1);
    assert!(!state.has_partial_message());
    assert!(!state.is_closed());
    assert_eq!(ledger.borrow().reserved, vec![source.len()]);
    assert_eq!(ledger.borrow().released, vec![source.len()]);
    assert_eq!(ledger.borrow().held, 0);
}

#[test]
fn a_three_fragment_message_reassembles_in_order() {
    let source = source_bytes(TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES * 2 + 3);
    let total = source.len() as u32;
    let max = TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES;
    let (mut state, _ledger) = assembler(usize::MAX);
    for (index, start) in [0, max, max * 2].into_iter().enumerate() {
        let end = (start + max).min(source.len());
        let fragment = state.push(
            &packet(1, total, start as u32, &source[start..end]),
            index as u64,
        );
        if end < source.len() {
            assert!(fragment.expect("a partial fragment is accepted").is_none());
        } else {
            let completed = fragment.expect("the last fragment completes");
            assert_eq!(completed.expect("returned"), source);
        }
    }
    assert_eq!(state.last_message_id(), 1);
}

#[test]
fn a_fragment_after_a_gap_is_refused_rather_than_spliced() {
    let (mut state, ledger) = assembler(usize::MAX);
    state
        .push(&packet(1, 6, 0, &[1, 2]), 0)
        .expect("the first fragment is retained");
    // The middle fragment never arrives, so the last cannot be appended behind it.
    let error = state
        .push(&packet(1, 6, 4, &[5, 6]), 1)
        .expect_err("a fragment that skips bytes is refused");
    assert_eq!(error, TerminalPeerPacketError::FragmentOrder);
    assert!(state.is_closed());
    assert!(!state.has_partial_message());
    assert_eq!(ledger.borrow().held, 0);
}

#[test]
fn a_reused_id_is_refused_and_a_single_packet_message_holds_no_reservation() {
    let (mut state, ledger) = assembler(usize::MAX);
    let single = state
        .push(&packet(1, 1, 0, &[1]), 0)
        .expect("a whole message in one fragment")
        .expect("returned");
    assert_eq!(single, vec![1u8]);
    // The copy reserves only while it copies: nothing is retained afterwards.
    assert_eq!(ledger.borrow().held, 0);
    let error = state
        .push(&packet(1, 1, 0, &[2]), 1)
        .expect_err("an id cannot be reused");
    assert_eq!(error, TerminalPeerPacketError::MessageId);
    assert!(state.is_closed());
    assert_eq!(state.last_message_id(), 1);
}

#[test]
fn a_refused_reservation_is_a_quota_rejection_that_releases_nothing() {
    let (mut state, ledger) = assembler(1);
    let error = state
        .push(&packet(1, 2, 0, &[1]), 1)
        .expect_err("the budget refuses two bytes");
    assert_eq!(error, TerminalPeerPacketError::Quota);
    assert!(state.is_closed());
    assert!(ledger.borrow().released.is_empty());
    assert_eq!(ledger.borrow().held, 0);
}

#[test]
fn expire_releases_a_stalled_partial_exactly_once() {
    let (mut state, ledger) = assembler(usize::MAX);
    state
        .push(&packet(1, 2, 0, &[1]), 100)
        .expect("the first fragment is retained");
    assert!(!state.expire(100 + TERMINAL_PEER_PACKET_STALL_MS - 1));
    assert!(!state.is_closed());
    assert!(state.expire(100 + TERMINAL_PEER_PACKET_STALL_MS));
    assert!(state.is_closed());
    assert!(!state.has_partial_message());
    assert_eq!(ledger.borrow().released, vec![2]);
    assert_eq!(ledger.borrow().held, 0);
}

#[test]
fn reset_clears_the_sequence_and_reopens_the_generation() {
    let (mut state, ledger) = assembler(usize::MAX);
    state
        .push(&packet(1, 2, 0, &[1]), 0)
        .expect("the first fragment is retained");
    state.reset();
    assert!(!state.has_partial_message());
    assert_eq!(state.last_message_id(), 0);
    assert_eq!(ledger.borrow().held, 0);
    let reopened = state
        .push(&packet(1, 1, 0, &[9]), 0)
        .expect("id 1 opens a fresh sequence")
        .expect("returned");
    assert_eq!(reopened, vec![9u8]);
}
