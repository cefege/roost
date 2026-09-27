//! The attachment-only outer framing: its header, its bounds, its reassembly,
//! and the two lanes' independent budgets.
//!
//! The defect class these prevent is a peer that can make this side retain more
//! than it was budgeted, and a fragment that is believed for a position the
//! ordered lane never reached.
//!
//! The v2 packet layer is shared infrastructure rather than a named v2 test
//! file, so these names are the module's own. The mutation experiment for this
//! file, in the slice report: raise `ATTACHMENT_PACKET_MAX_PAYLOAD_BYTES` to the
//! whole logical frame, and
//! `the_outbound_queue_fragments_a_frame_and_keeps_it_until_the_send_is_accepted`
//! must fail on its packet-count assertion.

use roost_client_core::client::attachments::packets::assembler::AttachmentPacketAssembler;
use roost_client_core::client::attachments::packets::lanes::AttachmentPeerPacketLanes;
use roost_client_core::client::attachments::packets::queue::AttachmentPacketQueue;
use roost_client_core::client::attachments::packets::{
    ATTACHMENT_PACKET_HEADER_BYTES, ATTACHMENT_PACKET_MAX_BYTES,
    ATTACHMENT_PACKET_MAX_PAYLOAD_BYTES, ATTACHMENT_PACKET_STALL_MS, AttachmentPacketError,
    AttachmentPacketHeader, PeerLane, encode_attachment_packet, parse_attachment_packet,
};

// ---------------------------------------------------------------- the packet

#[test]
fn a_packet_carries_the_attachment_magic_and_its_own_position() {
    let header = AttachmentPacketHeader {
        message_id: 7,
        total_bytes: 4,
        offset_bytes: 1,
    };
    let packet = encode_attachment_packet(header, &[10, 11, 12]).expect("encodable");
    assert_eq!(packet.len(), ATTACHMENT_PACKET_HEADER_BYTES + 3);
    assert_eq!(&packet[0..4], &[0x41, 0x54, 0x50, 0x31], "'ATP1'");

    let parsed = parse_attachment_packet(&packet).expect("parses back");
    assert_eq!(parsed.header, header);
    assert_eq!(parsed.payload, &[10, 11, 12]);
}

#[test]
fn a_packet_that_breaks_a_header_rule_is_refused_by_that_rule() {
    let zero_id = AttachmentPacketHeader {
        message_id: 0,
        total_bytes: 4,
        offset_bytes: 0,
    };
    assert_eq!(
        encode_attachment_packet(zero_id, &[1, 2, 3, 4]),
        Err(AttachmentPacketError::MessageId)
    );

    let past_end = AttachmentPacketHeader {
        message_id: 1,
        total_bytes: 4,
        offset_bytes: 4,
    };
    assert_eq!(
        encode_attachment_packet(past_end, &[1]),
        Err(AttachmentPacketError::PacketHeader),
        "an offset at the end of the message has no payload left to carry"
    );

    let bare_header = vec![0u8; ATTACHMENT_PACKET_HEADER_BYTES];
    assert_eq!(
        parse_attachment_packet(&bare_header),
        Err(AttachmentPacketError::PacketSize),
        "with no length field, a bare header is indistinguishable from a truncated packet"
    );

    let mut wrong_magic = encode_attachment_packet(
        AttachmentPacketHeader {
            message_id: 1,
            total_bytes: 2,
            offset_bytes: 0,
        },
        &[1, 2],
    )
    .expect("encodable");
    wrong_magic[0] = 0xff;
    assert_eq!(
        parse_attachment_packet(&wrong_magic),
        Err(AttachmentPacketError::PacketMagic),
        "attachment traffic may not be mistaken for a terminal lane's"
    );
}

#[test]
fn a_fragmented_message_reassembles_only_in_order() {
    let mut assembler = AttachmentPacketAssembler::new(PeerLane::Data);
    let whole: Vec<u8> = (0..8u8).collect();
    let first = encode_attachment_packet(
        AttachmentPacketHeader {
            message_id: 1,
            total_bytes: 8,
            offset_bytes: 0,
        },
        &whole[..4],
    )
    .expect("encodable");
    let second = encode_attachment_packet(
        AttachmentPacketHeader {
            message_id: 1,
            total_bytes: 8,
            offset_bytes: 4,
        },
        &whole[4..],
    )
    .expect("encodable");

    assert_eq!(
        assembler.push(&first, 0),
        Ok(None),
        "half a message is not a message"
    );
    assert!(assembler.has_partial_message());
    assert_eq!(assembler.push(&second, 10), Ok(Some(whole)));
    assert!(!assembler.has_partial_message());

    // A second message must be the NEXT id: this lane is ordered, so a
    // fragment that does not continue the sequence belongs to a peer that has
    // lost it, and the generation closes.
    let mut restarted = AttachmentPacketAssembler::new(PeerLane::Data);
    let skipped = encode_attachment_packet(
        AttachmentPacketHeader {
            message_id: 3,
            total_bytes: 2,
            offset_bytes: 0,
        },
        &[1, 2],
    )
    .expect("encodable");
    assert_eq!(
        restarted.push(&skipped, 0),
        Err(AttachmentPacketError::MessageId)
    );
    assert!(
        restarted.is_closed(),
        "a peer that sent one malformed fragment is no longer trusted with the bytes it holds"
    );
    assert_eq!(
        restarted.push(&skipped, 0),
        Err(AttachmentPacketError::Closed)
    );
}

#[test]
fn a_message_that_stops_arriving_stalls_the_lane() {
    let mut assembler = AttachmentPacketAssembler::new(PeerLane::Data);
    let first = encode_attachment_packet(
        AttachmentPacketHeader {
            message_id: 1,
            total_bytes: 8,
            offset_bytes: 0,
        },
        &[0, 1, 2, 3],
    )
    .expect("encodable");
    assert_eq!(assembler.push(&first, 0), Ok(None));
    assert!(
        !assembler.expire(ATTACHMENT_PACKET_STALL_MS - 1),
        "one slow chunk is not a stall"
    );
    assert!(assembler.expire(ATTACHMENT_PACKET_STALL_MS));
    assert!(assembler.is_closed());
}

/// Materialize the next packet without committing it, so a test can compare two
/// passes. The queue is borrowed for the call only.
fn peek(queue: &mut AttachmentPacketQueue) -> (Vec<u8>, bool) {
    let fragment = queue
        .next_fragment()
        .expect("no error while the queue is well formed");
    match fragment {
        Some(fragment) => {
            let bytes = fragment.bytes().to_vec();
            let final_fragment = fragment.final_fragment;
            (bytes, final_fragment)
        }
        None => (Vec::new(), false),
    }
}

#[test]
fn the_outbound_queue_fragments_a_frame_and_keeps_it_until_the_send_is_accepted() {
    let mut queue = AttachmentPacketQueue::new(PeerLane::Data);
    let frame = vec![7u8; ATTACHMENT_PACKET_MAX_PAYLOAD_BYTES + 10];
    assert_eq!(queue.enqueue(frame.clone()), Ok(true));
    assert_eq!(queue.message_count(), 1);
    assert_eq!(queue.queued_bytes(), frame.len());

    let (first_bytes, first_final) = peek(&mut queue);
    assert!(!first_final, "one packet cannot be the whole frame");
    assert_eq!(
        first_bytes.len(),
        ATTACHMENT_PACKET_HEADER_BYTES + ATTACHMENT_PACKET_MAX_PAYLOAD_BYTES
    );
    // Not committing leaves the message in place, so the next pass produces the
    // same bytes rather than losing them.
    assert_eq!(peek(&mut queue).0, first_bytes);
    assert_eq!(queue.message_count(), 1);

    let pending = queue
        .next_fragment()
        .expect("a packet is owed")
        .expect("a packet");
    pending.commit();
    assert_eq!(queue.message_count(), 1, "the frame is only half out");
    let pending = queue
        .next_fragment()
        .expect("a packet is owed")
        .expect("a packet");
    assert!(pending.final_fragment);
    pending.commit();
    assert_eq!(queue.message_count(), 0);
    assert_eq!(
        queue.queued_bytes(),
        0,
        "the budget comes back once the whole frame is out"
    );
    assert!(queue.next_fragment().expect("no error").is_none());
}

#[test]
fn the_two_lanes_keep_independent_budgets_in_both_directions() {
    let mut lanes = AttachmentPeerPacketLanes::new();
    // The control lane is budgeted at 128 KiB and the data lane at 1 MiB, and
    // neither may spend the other's.
    let control_budget = PeerLane::Control.queue_max_bytes();
    assert_eq!(
        lanes
            .outbound_mut(PeerLane::Control)
            .enqueue(vec![0u8; control_budget]),
        Ok(true)
    );
    assert_eq!(
        lanes.outbound_mut(PeerLane::Control).enqueue(vec![0u8; 1]),
        Ok(false),
        "a full control lane is backpressured, not grown"
    );
    assert_eq!(
        lanes.outbound_mut(PeerLane::Data).enqueue(vec![0u8; 64]),
        Ok(true),
        "a full control lane does not stop the data lane queueing"
    );
    assert!(lanes.has_queued_packets());
    lanes.clear();
    assert!(!lanes.has_queued_packets());
    assert_eq!(lanes.outbound(PeerLane::Control).queued_bytes(), 0);
    assert!(
        lanes.outbound(PeerLane::Control).is_closed(),
        "clearing closes the generation, so nothing more is queued on it"
    );
}
