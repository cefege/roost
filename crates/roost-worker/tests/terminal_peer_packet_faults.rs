//! The smoke harness's packet faults at the port boundary: an authenticated
//! port with the blackhole set spends its fragments without sending them, a
//! paused history lane never holds back control, and an injected malformed
//! control packet closes only an authenticated port. Pins
//! `peer::packet_test_faults` and the flush hooks in `peer::packet_lanes`
//! (v2 `terminal-peer-packet-port.ts` `shouldBlackholeOutgoing`,
//! `setHistoryDeliveryPausedForTest`, `injectMalformedPacketForTest`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "peer_support/fake_native.rs"]
mod fake_native;
#[path = "peer_support/packet_port_fixture.rs"]
mod packet_port_fixture;

use std::sync::Arc;

use packet_port_fixture::fixture_with_faults;
use roost_protocol::terminal_peer::packets::TerminalPeerPacketLane as Lane;
use roost_worker::local_terminal::{PeerTerminalPacketPort, TerminalPacketPort};
use roost_worker::peer::{MalformedPacket, PeerTestFaults};

#[tokio::test]
async fn a_blackholed_authenticated_port_drains_its_lanes_without_sending() {
    let faults = Arc::new(PeerTestFaults::default());
    let fixture = fixture_with_faults(Arc::clone(&faults));
    faults.set_packet_blackhole(true);

    // Before the Hello matched, the port still talks: the blackhole models a
    // peer that went silent after it was proven.
    fixture.port.send(vec![1], Lane::Control);
    assert_eq!(fixture.native.sent(Lane::Control as usize).len(), 1);

    fixture.port.mark_authenticated();
    fixture.port.send(vec![2], Lane::Control);
    fixture.port.send(vec![3], Lane::Terminal);
    assert_eq!(fixture.native.sent(Lane::Control as usize).len(), 1);
    assert!(fixture.native.sent(Lane::Terminal as usize).is_empty());
    fixture.port.wait_for_lane_drain(Lane::Terminal).await;
    assert!(fixture.port.is_open(), "silence is not a close");

    faults.set_packet_blackhole(false);
    fixture.port.send(vec![4], Lane::Terminal);
    assert_eq!(fixture.native.sent(Lane::Terminal as usize).len(), 1);
}

#[tokio::test]
async fn a_paused_history_lane_holds_history_but_never_control() {
    let fixture = fixture_with_faults(Arc::new(PeerTestFaults::default()));
    fixture.port.set_history_paused(true);
    fixture.port.send(vec![9], Lane::History);
    fixture.port.send(vec![1], Lane::Control);
    assert!(fixture.native.sent(Lane::History as usize).is_empty());
    assert_eq!(fixture.native.sent(Lane::Control as usize).len(), 1);

    fixture.port.set_history_paused(false);
    assert_eq!(
        fixture.native.sent(Lane::History as usize).len(),
        1,
        "resuming flushes what the pause held"
    );
}

#[tokio::test]
async fn a_malformed_control_packet_closes_only_an_authenticated_port() {
    for packet in [
        MalformedPacket::Offset,
        MalformedPacket::Total,
        MalformedPacket::Id,
    ] {
        let fixture = fixture_with_faults(Arc::new(PeerTestFaults::default()));
        assert!(
            !fixture.port.inject_malformed_control(packet),
            "an unauthenticated port is not a target"
        );
        assert!(fixture.close_reasons().is_empty());

        fixture.port.mark_authenticated();
        assert!(fixture.port.inject_malformed_control(packet));
        assert_eq!(fixture.close_reasons(), vec!["packet_rejected".to_owned()]);
        assert!(
            fixture.messages().is_empty(),
            "{packet:?} never reached ingress"
        );
        assert!(
            !fixture.port.inject_malformed_control(packet),
            "a closed port is not a target"
        );
    }
}
