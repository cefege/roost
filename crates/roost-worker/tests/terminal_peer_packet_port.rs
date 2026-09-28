//! Terminal peer packet port egress over fake native channels: ownership at
//! the native false-send boundary, the control reservation that keeps
//! history from blocking control, lane priority, the per-turn flush bound,
//! history drain and pre-read reservation, and pressure retirement. Ports v2
//! `apps/worker/tests/terminal/peer/terminal-peer-packet-port.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "peer_support/fake_native.rs"]
mod fake_native;
#[path = "peer_support/packet_port_fixture.rs"]
mod packet_port_fixture;

use packet_port_fixture::{Fixture, fixture, fixture_with};
use roost_protocol::terminal_peer::packets::{
    TerminalPeerPacketLane as Lane, TerminalPeerPacketQuota, parse_terminal_peer_packet,
};
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES, TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES,
    TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN, TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
};
use roost_worker::local_terminal::{PacketSendResult, PeerTerminalPacketPort, TerminalPacketPort};
use roost_worker::peer::{PacketDirection, TerminalPeerPacketBudget};

#[tokio::test]
async fn commits_a_native_false_return_once_without_retrying_its_accepted_fragment() {
    let fixture = fixture();
    fixture.native.push_send_result(0, false);
    assert_eq!(
        fixture.port.send(vec![7, 8, 9], Lane::Control),
        PacketSendResult::Backpressured
    );
    let sent = fixture.native.sent(0);
    assert_eq!(sent.len(), 1);
    let packet = parse_terminal_peer_packet(Lane::Control, &sent[0]).unwrap();
    assert_eq!(
        (
            packet.header.message_id,
            packet.header.total_bytes,
            packet.payload
        ),
        (1, 3, &[7u8, 8, 9][..])
    );
    fixture.emit_low(Lane::Control);
    assert_eq!(fixture.native.sent(0).len(), 1);
    assert_eq!(
        fixture.port.send(vec![1], Lane::Control),
        PacketSendResult::Accepted
    );
    assert_eq!(fixture.native.sent(0).len(), 2);
}

#[tokio::test]
async fn keeps_control_reservation_separate_from_application_bytes_and_drains_lanes_by_priority() {
    let budget = TerminalPeerPacketBudget::new();
    let peer = budget.create_peer_budget().unwrap();
    let mut history = peer.quota(PacketDirection::Outgoing, Lane::History);
    let mut control = peer.quota(PacketDirection::Outgoing, Lane::Control);
    assert!(history.reserve(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES));
    assert!(control.reserve(TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES));
    assert!(!history.reserve(1));
    assert!(!control.reserve(1));
    control.release(TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES);
    history.release(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES);
    let worker = TerminalPeerPacketBudget::new();
    let mut quotas: Vec<_> = (0..3)
        .map(|_| {
            worker
                .create_peer_budget()
                .unwrap()
                .quota(PacketDirection::Outgoing, Lane::History)
        })
        .collect();
    assert!(quotas[0].reserve(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES));
    assert!(quotas[1].reserve(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES));
    assert!(
        !quotas[2].reserve(1),
        "the worker's application ceiling refuses a third peer"
    );
    quotas[0].release(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES);
    quotas[1].release(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES);

    let fixture = fixture();
    for lane in Lane::ALL {
        fixture.saturate(lane);
    }
    assert_eq!(
        fixture.port.send(vec![1], Lane::History),
        PacketSendResult::Backpressured
    );
    assert_eq!(
        fixture.port.send(vec![2], Lane::Terminal),
        PacketSendResult::Backpressured
    );
    assert_eq!(
        fixture.port.send(vec![3], Lane::Control),
        PacketSendResult::Backpressured
    );
    for lane in Lane::ALL {
        fixture.native.set_buffered(lane as usize, 0);
    }
    fixture.emit_low(Lane::Control);
    assert_eq!(fixture.native.sent_order(), vec![0, 1, 2]);
}

#[tokio::test]
async fn yields_before_sending_more_than_64_kib_from_one_low_water_flush() {
    let fixture = fixture();
    fixture.saturate(Lane::Control);
    let max = TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES;
    for size in [max, max, max, 10_000, max] {
        fixture.port.send(vec![0; size], Lane::Control);
    }
    fixture.emit_low(Lane::Control);
    let sent = fixture.native.sent(0);
    assert!(sent.iter().map(Vec::len).sum::<usize>() <= TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN);
    assert_eq!(sent.len(), 4);
    fixture.port.close(1000, "test");
}

#[tokio::test]
async fn settles_a_history_admission_only_after_its_queued_final_fragment_drains() {
    let fixture = fixture();
    fixture.saturate(Lane::History);
    assert_eq!(
        fixture.port.send(vec![9], Lane::History),
        PacketSendResult::Backpressured
    );
    let drained = fixture.port.wait_for_lane_drain(Lane::History);
    fixture.emit_low(Lane::History);
    drained.await;
    assert_eq!(fixture.native.sent(2).len(), 1);
}

#[tokio::test]
async fn transfers_a_pre_read_history_ceiling_into_exact_queue_ownership() {
    let fixture = fixture();
    fixture.saturate(Lane::History);
    let reserved = TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES - 4 * 1024;
    let mut reservation = fixture
        .port
        .reserve_history_read(reserved)
        .expect("history pre-read reservation");
    let outgoing = |fixture: &Fixture| {
        fixture
            .budget
            .snapshot(PacketDirection::Outgoing)
            .application_bytes
    };
    assert_eq!(outgoing(&fixture), reserved);
    assert_eq!(
        fixture.port.send(vec![7], Lane::History),
        PacketSendResult::Backpressured
    );
    assert_eq!(outgoing(&fixture), reserved + 1);
    reservation.transfer();
    assert_eq!(
        fixture.port.send(vec![9], Lane::History),
        PacketSendResult::Backpressured
    );
    assert_eq!(outgoing(&fixture), 2);
    drop(reservation);
    assert_eq!(outgoing(&fixture), 2);
    fixture.emit_low(Lane::History);
    fixture.port.wait_for_lane_drain(Lane::History).await;
    assert_eq!(outgoing(&fixture), 0);

    let orphan = self::fixture();
    let reservation = orphan
        .port
        .reserve_history_read(reserved)
        .expect("orphan history reservation");
    orphan.port.close(1000, "test");
    assert_eq!(
        outgoing(&orphan),
        reserved,
        "a read in flight keeps its bytes charged past close"
    );
    drop(reservation);
    assert_eq!(outgoing(&orphan), 0);
}

/// v2 `terminal-peer-history-reservation.ts:47-57`: a read that ends without
/// transferring (it failed or was cancelled) returns its whole ceiling to the
/// quota of a port that stays open, so the next read can reserve it again.
/// The closed-port case above cannot show this: disposal returns everything.
#[tokio::test]
async fn an_untransferred_read_returns_its_ceiling_to_an_open_port() {
    let fixture = fixture();
    let ceiling = TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES;
    let outgoing = || {
        fixture
            .budget
            .snapshot(PacketDirection::Outgoing)
            .application_bytes
    };
    let first = fixture.port.reserve_history_read(ceiling).expect("first read");
    assert_eq!(outgoing(), ceiling);
    drop(first);
    assert!(fixture.port.is_open());
    assert_eq!(outgoing(), 0);
    let second = fixture
        .port
        .reserve_history_read(ceiling)
        .expect("the released ceiling admits the next read");
    assert_eq!(outgoing(), ceiling);
    drop(second);
    fixture.port.close(1000, "test");
}

#[tokio::test]
async fn retires_a_history_holder_instead_of_the_healthy_terminal_sender_under_worker_pressure() {
    let worker = TerminalPeerPacketBudget::new();
    let first = fixture_with(worker.clone());
    let second = fixture_with(worker.clone());
    let healthy = fixture_with(worker);
    let first_reservation = first
        .port
        .reserve_history_read(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES)
        .expect("first");
    let second_reservation = second
        .port
        .reserve_history_read(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES)
        .expect("second");

    assert_eq!(
        healthy.port.send(vec![0; 16 * 1024], Lane::Terminal),
        PacketSendResult::Accepted
    );
    assert!(healthy.port.is_open());
    assert!(!first.port.is_open());
    assert!(
        first
            .close_reasons()
            .contains(&"application_pressure".to_owned())
    );
    assert!(second.port.is_open());

    drop(first_reservation);
    drop(second_reservation);
    second.port.close(1000, "test");
    healthy.port.close(1000, "test");
}

/// Budgets refuse an over-limit packet: a control frame beyond the peer's
/// control ceiling is refused at admission, not queued.
#[tokio::test]
async fn a_send_beyond_the_peer_control_ceiling_is_refused() {
    let fixture = fixture();
    fixture.saturate(Lane::Control);
    let frame = 128 * 1024;
    assert_eq!(
        fixture.port.send(vec![0; frame], Lane::Control),
        PacketSendResult::Backpressured
    );
    assert_eq!(
        fixture.port.send(vec![0; frame], Lane::Control),
        PacketSendResult::Backpressured
    );
    assert_eq!(
        fixture.port.send(vec![0; 1], Lane::Control),
        PacketSendResult::Refused
    );
    assert_eq!(
        fixture
            .budget
            .snapshot(PacketDirection::Outgoing)
            .control_bytes,
        TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES
    );
}
