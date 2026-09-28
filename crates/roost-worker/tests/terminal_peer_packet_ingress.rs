//! Terminal peer packet port ingress: framed control reassembly, malformed
//! packet rejection with its partial quota released on close, and refusal of
//! client data off the control lane. Ports the receive cases of v2
//! `apps/worker/tests/terminal/peer/terminal-peer-packet-port.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "peer_support/fake_native.rs"]
mod fake_native;
#[path = "peer_support/packet_port_fixture.rs"]
mod packet_port_fixture;

use packet_port_fixture::{fixture, framed_control};
use roost_protocol::terminal_peer::packets::{
    TerminalPeerPacketHeader, TerminalPeerPacketLane as Lane, encode_terminal_peer_packet,
};
use roost_protocol::terminal_peer::peer::TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES;
use roost_worker::local_terminal::{PeerTerminalPacketPort, TerminalPacketPort};
use roost_worker::peer::PacketDirection;

#[tokio::test]
async fn reassembles_framed_control_rejects_malformed_packets_and_releases_a_partial_quota_on_close()
 {
    let fixture = fixture();
    fixture.emit_message(Lane::Control, framed_control(1, &[4, 5]));
    assert_eq!(fixture.messages(), vec![vec![4, 5]]);
    let unauthenticated = self::fixture();
    unauthenticated.emit_message(Lane::Control, framed_control(1, &[0; 4_097]));
    assert!(!unauthenticated.port.is_open());
    assert_eq!(
        unauthenticated.close_reasons(),
        vec!["unauthenticated_frame_too_large"]
    );

    fixture.port.mark_authenticated();
    let total = TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES + 1;
    let header = TerminalPeerPacketHeader {
        message_id: 2,
        total_bytes: total as u32,
        offset_bytes: 0,
    };
    let first = encode_terminal_peer_packet(
        Lane::Control,
        header,
        &vec![3; TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES],
    )
    .unwrap();
    fixture.emit_message(Lane::Control, first);
    assert_eq!(
        fixture
            .budget
            .snapshot(PacketDirection::Incoming)
            .control_bytes,
        total
    );
    fixture.port.close(1000, "test");
    assert_eq!(
        fixture
            .budget
            .snapshot(PacketDirection::Incoming)
            .retained_bytes,
        0
    );

    let healthy = self::fixture();
    let mut malformed = framed_control(1, &[1]);
    malformed[0..4].copy_from_slice(&0u32.to_le_bytes());
    healthy.emit_message(Lane::Control, malformed);
    assert!(!healthy.port.is_open());
    assert_eq!(healthy.close_reasons(), vec!["packet_rejected"]);
}

#[tokio::test]
async fn refuses_non_control_client_data_before_it_can_reach_a_terminal_ingress() {
    let fixture = fixture();
    fixture.emit_message(Lane::Terminal, framed_control(1, &[1]));
    assert!(!fixture.port.is_open());
    assert!(fixture.messages().is_empty());
    assert_eq!(fixture.close_reasons(), vec!["unexpected_client_data"]);
}
