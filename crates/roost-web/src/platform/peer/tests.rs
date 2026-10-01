use super::{BrowserPeer, lane_slot};
use roost_client_core::client::carriers::PeerLane;

#[test]
fn a_fresh_adapter_holds_no_peer_and_says_so_to_the_document_cap() {
    let peer = BrowserPeer::new();
    assert_eq!(peer.open_count(), 0);
    assert!(peer.attempt_of(1).is_none());
    assert!(!peer.holds(1));
    assert!(!peer.lane_is_open(1, PeerLane::Control));
    assert!(!peer.has_answer(1));
}

#[test]
fn each_lane_reads_its_own_slot_and_no_other() {
    let mut slots = [false; 3];
    slots[PeerLane::Control.stream_id() as usize] = true;
    assert!(lane_slot(slots, PeerLane::Control));
    assert!(!lane_slot(slots, PeerLane::Data));
    assert!(!lane_slot(slots, PeerLane::History));

    slots[PeerLane::History.stream_id() as usize] = true;
    assert!(lane_slot(slots, PeerLane::History));
    assert!(
        lane_slot(slots, PeerLane::Control),
        "one lane opening says nothing about another lane's readiness"
    );
}
