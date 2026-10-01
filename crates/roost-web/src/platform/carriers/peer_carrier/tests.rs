use super::{LaneBudget, LaneFault, PeerLanes, drain_order, lane_cap, watermarks};
use roost_client_core::client::carriers::PeerLane;
use roost_protocol::terminal_peer::packets::TerminalPeerPacketQuota;

#[test]
fn a_message_that_fits_one_packet_survives_the_queue_and_the_assembler() {
    let mut lanes = PeerLanes::new(4);
    assert!(
        lanes
            .enqueue(PeerLane::Control, b"hello".to_vec())
            .expect("the control lane accepts a short message")
    );

    let written = drain_outbound(&mut lanes, PeerLane::Control);
    assert_eq!(written.len(), 1, "a message this small is one packet");
    assert_eq!(
        lanes
            .push(PeerLane::Control, 0, &written[0])
            .expect("the packet parses"),
        Some(b"hello".to_vec()),
        "one whole packet is one whole message"
    );
}

#[test]
fn a_message_larger_than_one_packet_is_reassembled_across_fragments() {
    let mut lanes = PeerLanes::new(1);
    let payload: Vec<u8> = (0..40_000u32).map(|index| index as u8).collect();
    lanes
        .enqueue(PeerLane::Data, payload.clone())
        .expect("the terminal lane accepts a whole frame");

    let packets = drain_outbound(&mut lanes, PeerLane::Data);
    assert!(packets.len() > 1, "this payload cannot fit one packet");

    let mut assembled = Vec::new();
    for packet in &packets {
        if let Some(message) = lanes
            .push(PeerLane::Data, 0, packet)
            .expect("the packet parses")
        {
            assembled.push(message);
        }
    }
    assert_eq!(assembled, vec![payload]);
}

#[test]
fn a_fragment_that_does_not_continue_the_message_is_refused_as_out_of_order() {
    let mut lanes = PeerLanes::new(1);
    lanes
        .enqueue(PeerLane::Data, vec![7; 40_000])
        .expect("the terminal lane accepts a whole frame");
    let packets = drain_outbound(&mut lanes, PeerLane::Data);
    assert!(packets.len() > 1, "this payload is split");

    // The SECOND fragment before the first. The bytes parse, so this is not
    // an unreadable packet: it is an ordered lane that lost its sequence,
    // which retires the peer rather than losing one message.
    assert_eq!(
        lanes.push(PeerLane::Data, 0, &packets[1]),
        Err(LaneFault::OutOfOrder {
            lane: PeerLane::Data
        })
    );
}

#[test]
fn bytes_that_are_not_a_packet_are_refused_by_lane() {
    let mut lanes = PeerLanes::new(1);
    assert_eq!(
        lanes.push(PeerLane::Control, 0, b"not a packet at all"),
        Err(LaneFault::Unreadable {
            lane: PeerLane::Control,
            detail: "packet-magic".to_owned(),
        })
    );
}

#[test]
fn a_message_larger_than_its_lane_cap_is_refused_rather_than_fragmented_forever() {
    let mut lanes = PeerLanes::new(1);
    assert!(
        matches!(
            lanes.enqueue(PeerLane::Control, vec![0; lane_cap(PeerLane::Control) + 1]),
            Err(LaneFault::Unreadable { .. })
        ),
        "a message the lane's cap forbids never enters its queue"
    );
}

#[test]
fn a_second_history_read_is_refused_because_the_peer_is_allowed_one() {
    let mut lanes = PeerLanes::new(1);
    assert!(lanes.enqueue(PeerLane::History, b"page".to_vec()).is_ok());
    assert_eq!(
        lanes.enqueue(PeerLane::History, b"page again".to_vec()),
        Err(LaneFault::Backpressured {
            lane: PeerLane::History
        }),
        "one history read per peer, so a second is refused rather than queued behind it"
    );
}

#[test]
fn one_peer_may_hold_one_whole_message_on_each_lane_and_no_more() {
    let mut budget = LaneBudget::new(PeerLane::Control);
    let cap = lane_cap(PeerLane::Control);
    assert!(budget.reserve(cap));
    assert_eq!(budget.retained(), cap);
    assert!(
        !budget.reserve(cap),
        "two whole control messages exceed what one peer may retain"
    );
    budget.release(cap);
    assert_eq!(
        budget.retained(),
        0,
        "a released reservation is available again"
    );
}

#[test]
fn the_drain_order_puts_the_keystroke_lane_ahead_of_the_baseline_lane() {
    assert_eq!(
        drain_order(),
        [PeerLane::Control, PeerLane::Data, PeerLane::History],
        "a keystroke must not sit behind a multi-megabyte baseline"
    );
    let control = watermarks(PeerLane::Control);
    let history = watermarks(PeerLane::History);
    assert!(
        control.0 < history.0,
        "the control lane saturates far below the history lane, which is the point"
    );
}

/// Every queued fragment of one lane, committed as it is written.
fn drain_outbound(lanes: &mut PeerLanes, lane: PeerLane) -> Vec<Vec<u8>> {
    let mut written = Vec::new();
    while let Some(fragment) = lanes.next_fragment(lane).expect("the lane has a queue") {
        written.push(fragment.bytes().to_vec());
        fragment.commit();
    }
    written
}
