//! The bounded send queue in front of the assembler.
//!
//! The point of the queue is that a stalled peer cannot make a worker retain
//! terminal output without limit, so the tests pin what a refused send hands
//! back to the caller and what survives a full queue: the oldest message stays
//! first, because dropping a prefix of an ordered lane splices the sequence.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use roost_protocol::terminal_peer::packet_queue::TerminalPeerPacketQueue;
use roost_protocol::terminal_peer::packets::{
    TERMINAL_PEER_PACKET_HEADER_BYTES, TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
    TerminalPeerPacketError, TerminalPeerPacketQuota, parse_terminal_peer_packet,
};
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_SCTP_MAX_CHUNKS_ON_QUEUE, TerminalPeerLaneByteCaps, TerminalPeerPacketLane,
};

const LANE: TerminalPeerPacketLane = TerminalPeerPacketLane::Control;

#[derive(Default)]
struct RetainedBytes {
    held: usize,
    reserved: Vec<usize>,
    released: Vec<usize>,
}

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

fn queue(limit: usize) -> (TerminalPeerPacketQueue<RecordingQuota>, Ledger) {
    let ledger: Ledger = Rc::new(RefCell::new(RetainedBytes::default()));
    let quota = RecordingQuota {
        limit,
        ledger: Rc::clone(&ledger),
    };
    (TerminalPeerPacketQueue::new(LANE, quota), ledger)
}

#[test]
fn a_queue_keeps_whole_messages_and_fragments_one_packet_at_a_time() {
    let (mut queue, ledger) = queue(usize::MAX);
    let first = vec![1u8; TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES + 1];
    let second = vec![2u8, 3u8];
    assert!(queue.enqueue(first.clone()).expect("admitted"));
    assert!(queue.enqueue(second.clone()).expect("admitted"));
    assert_eq!(queue.queued_bytes(), first.len() + second.len());
    assert_eq!(queue.message_count(), 2);
    assert_eq!(queue.lane, LANE);

    let head = queue
        .next_fragment()
        .expect("the queue is open")
        .expect("a fragment is materialized");
    assert_eq!(head.message_id, 1);
    assert!(!head.final_fragment);
    assert_eq!(
        head.bytes().len(),
        TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES + TERMINAL_PEER_PACKET_HEADER_BYTES
    );
    let parsed = parse_terminal_peer_packet(LANE, head.bytes()).expect("a framed packet parses");
    assert_eq!(parsed.header.total_bytes, first.len() as u32);
    assert_eq!(parsed.header.offset_bytes, 0);
    head.commit();

    let tail = queue
        .next_fragment()
        .expect("the queue is open")
        .expect("the tail is materialized");
    assert!(tail.final_fragment);
    let parsed = parse_terminal_peer_packet(LANE, tail.bytes()).expect("a framed packet parses");
    assert_eq!(
        parsed.header.offset_bytes,
        TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES as u32
    );
    assert_eq!(parsed.payload, &[1u8]);
    tail.commit();

    let whole = queue
        .next_fragment()
        .expect("the queue is open")
        .expect("the second message is next");
    assert_eq!(whole.message_id, 2);
    assert!(whole.final_fragment);
    assert_eq!(whole.bytes()[TERMINAL_PEER_PACKET_HEADER_BYTES..], second);
    whole.commit();

    assert!(queue.next_fragment().expect("the queue is open").is_none());
    assert_eq!(queue.queued_bytes(), 0);
    assert_eq!(queue.message_count(), 0);
    assert_eq!(ledger.borrow().released, vec![first.len(), second.len()]);
    assert_eq!(ledger.borrow().held, 0);
}

#[test]
fn the_byte_bound_refuses_a_send_before_ownership_transfers() {
    let (mut queue, ledger) = queue(1);
    let refused = queue
        .enqueue(vec![1u8, 2u8])
        .expect("a refusal is not an error");
    assert!(!refused, "the caller is told the lane is full");
    assert_eq!(queue.message_count(), 0);
    assert_eq!(queue.queued_bytes(), 0);
    assert!(queue.next_fragment().expect("the queue is open").is_none());
    assert!(!queue.is_closed());
    // A refused send leaves the budget exactly as it found it.
    assert_eq!(ledger.borrow().held, 0);
    assert!(ledger.borrow().released.is_empty());
}

#[test]
fn a_full_queue_keeps_its_oldest_message_and_refuses_the_newest() {
    let (mut queue, _ledger) = queue(4);
    assert!(queue.enqueue(vec![1u8, 1u8]).expect("admitted"));
    assert!(queue.enqueue(vec![2u8, 2u8]).expect("admitted"));
    assert!(
        !queue
            .enqueue(vec![3u8, 3u8])
            .expect("a refusal is not an error")
    );
    assert_eq!(queue.message_count(), 2);
    assert_eq!(queue.queued_bytes(), 4);
    let first = queue
        .next_fragment()
        .expect("the queue is open")
        .expect("materialized");
    assert_eq!(first.message_id, 1);
    first.commit();
    let second = queue
        .next_fragment()
        .expect("the queue is open")
        .expect("materialized");
    assert_eq!(second.message_id, 2);
    second.commit();
    assert!(queue.next_fragment().expect("the queue is open").is_none());
}

#[test]
fn a_message_outside_the_lane_cap_or_of_no_length_is_refused() {
    let (mut queue, _ledger) = queue(usize::MAX);
    let over = TerminalPeerLaneByteCaps::for_lane(LANE) + 1;
    let error = queue
        .enqueue(vec![0u8; over])
        .expect_err("the control lane cannot carry that much");
    assert_eq!(error, TerminalPeerPacketError::MessageSize);
    let error = queue
        .enqueue(Vec::new())
        .expect_err("an empty message has no fragments to send");
    assert_eq!(error, TerminalPeerPacketError::MessageSize);
    assert_eq!(queue.message_count(), 0);
}

#[test]
fn an_uncommitted_fragment_is_materialized_again_unchanged() {
    let (mut queue, _ledger) = queue(usize::MAX);
    queue.enqueue(vec![7u8, 8u8, 9u8]).expect("admitted");
    let abandoned = queue
        .next_fragment()
        .expect("the queue is open")
        .expect("materialized");
    let first_bytes = abandoned.bytes().to_vec();
    let again = queue
        .next_fragment()
        .expect("the queue is open")
        .expect("materialized again");
    assert_eq!(again.bytes(), first_bytes.as_slice());
    // Only the commit advances the queue. The byte count cannot be read while
    // the guard is alive — the guard owns the queue for exactly as long as the
    // fragment is outstanding — so the proof is that the same three bytes come
    // back, and that the queue is empty once the commit lands.
    again.commit();
    assert_eq!(queue.queued_bytes(), 0);
    assert_eq!(queue.message_count(), 0);
}

#[test]
fn clear_is_terminal_until_reset_opens_a_fresh_sequence() {
    let (mut queue, ledger) = queue(usize::MAX);
    queue.enqueue(vec![1u8]).expect("admitted");
    let fragment = queue
        .next_fragment()
        .expect("the queue is open")
        .expect("materialized");
    fragment.commit();
    queue.enqueue(vec![2u8]).expect("admitted");
    queue.clear();
    queue.clear();
    assert!(queue.is_closed());
    assert_eq!(queue.message_count(), 0);
    assert_eq!(queue.queued_bytes(), 0);
    // One release per message, however many times clear runs.
    assert_eq!(ledger.borrow().released, vec![1, 1]);
    let error = queue
        .enqueue(vec![3u8])
        .expect_err("a closed queue takes no messages");
    assert_eq!(error, TerminalPeerPacketError::Closed);

    queue.reset();
    assert!(!queue.is_closed());
    queue.enqueue(vec![4u8]).expect("admitted");
    let fragment = queue
        .next_fragment()
        .expect("the queue is open")
        .expect("materialized");
    let parsed = parse_terminal_peer_packet(LANE, fragment.bytes()).expect("parses");
    assert_eq!(parsed.header.message_id, 1);
    fragment.commit();
}

#[test]
fn admission_is_decided_by_bytes_not_by_a_message_count() {
    // The queue bounds retained bytes one logical message at a time. The count
    // of chunks in flight on the wire is the transport's bound, so a queue
    // that refused on message count would be refusing a limit it cannot see.
    let (mut queue, _ledger) = queue(usize::MAX);
    let messages = TERMINAL_PEER_SCTP_MAX_CHUNKS_ON_QUEUE + 1;
    for message in 0..messages {
        queue
            .enqueue(vec![message as u8])
            .expect("a generous byte budget admits every message");
    }
    assert_eq!(queue.message_count(), messages);
    assert_eq!(queue.queued_bytes(), messages);
}
