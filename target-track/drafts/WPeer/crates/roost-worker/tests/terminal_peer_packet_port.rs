//! Terminal peer packet port over fake native channels: ownership at the
//! native false-send boundary, frame rejection, the control reservation that
//! keeps history from blocking control, lane priority, the per-turn flush
//! bound, history drain and pre-read reservation, pressure retirement, and
//! refusal of client data off the control lane. Ports v2
//! `apps/worker/tests/terminal/peer/terminal-peer-packet-port.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "peer_support/fake_native.rs"]
mod fake_native;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fake_native::FakePeer;
use roost_protocol::terminal_peer::packets::{
    TerminalPeerPacketHeader, TerminalPeerPacketLane as Lane, TerminalPeerPacketQuota,
    encode_terminal_peer_packet, parse_terminal_peer_packet,
};
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES, TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES,
    TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN, TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
    TerminalPeerChannelWatermarks,
};
use roost_worker::local_terminal::{PacketSendResult, PeerTerminalPacketPort, TerminalPacketPort};
use roost_worker::peer::native::NativePeerEvent;
use roost_worker::peer::{
    PacketDirection, PacketPortDeps, TerminalPeerPacketBudget, TerminalPeerPacketIngress,
    TerminalPeerPacketPort,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Recorded {
    messages: Mutex<Vec<Vec<u8>>>,
    close_reasons: Mutex<Vec<String>>,
}

struct RecordingIngress(Arc<Recorded>);

impl TerminalPeerPacketIngress for RecordingIngress {
    fn on_message(&self, bytes: &[u8]) {
        lock(&self.0.messages).push(bytes.to_vec());
    }
    fn on_close(&self) {}
}

struct Fixture {
    budget: TerminalPeerPacketBudget,
    native: Arc<FakePeer>,
    port: Arc<TerminalPeerPacketPort>,
    recorded: Arc<Recorded>,
}

impl Fixture {
    fn messages(&self) -> Vec<Vec<u8>> {
        lock(&self.recorded.messages).clone()
    }
    fn close_reasons(&self) -> Vec<String> {
        lock(&self.recorded.close_reasons).clone()
    }
    fn emit(&self, event: NativePeerEvent) {
        self.port.on_channel_event(event);
    }
    /// v2 `channel.emitLow()`.
    fn emit_low(&self, lane: Lane) {
        self.native.set_buffered(lane as usize, 0);
        self.emit(NativePeerEvent::BufferedAmountLow(lane as usize));
    }
    fn emit_message(&self, lane: Lane, data: Vec<u8>) {
        self.emit(NativePeerEvent::ChannelMessage { channel: lane as usize, binary: true, data });
    }
    fn saturate(&self, lane: Lane) {
        self.native.set_buffered(lane as usize, TerminalPeerChannelWatermarks::for_lane(lane).0);
    }
}

fn fixture_with(budget: TerminalPeerPacketBudget) -> Fixture {
    let native = FakePeer::standalone(3);
    let recorded = Arc::new(Recorded::default());
    let closes = Arc::clone(&recorded);
    let port = TerminalPeerPacketPort::new(PacketPortDeps {
        socket_id: "peer-socket".into(),
        native: Arc::clone(&native) as _,
        budget: budget.clone(),
        peer_budget: budget.create_peer_budget().unwrap(),
        on_closed: Some(Arc::new(move |reason: &str| lock(&closes.close_reasons).push(reason.to_owned()))),
        on_fatal: None,
        runtime: tokio::runtime::Handle::current(),
    });
    assert!(port.attach_ingress(Arc::new(RecordingIngress(Arc::clone(&recorded)))));
    for channel in 0..3 {
        native.set_open(channel, true);
        port.on_channel_event(NativePeerEvent::ChannelOpen(channel));
    }
    Fixture { budget, native, port, recorded }
}

fn fixture() -> Fixture {
    fixture_with(TerminalPeerPacketBudget::new())
}

fn framed_control(message_id: u32, payload: &[u8]) -> Vec<u8> {
    let header = TerminalPeerPacketHeader { message_id, total_bytes: payload.len() as u32, offset_bytes: 0 };
    encode_terminal_peer_packet(Lane::Control, header, payload).unwrap()
}

#[tokio::test]
async fn commits_a_native_false_return_once_without_retrying_its_accepted_fragment() {
    let fixture = fixture();
    fixture.native.push_send_result(0, false);
    assert_eq!(fixture.port.send(vec![7, 8, 9], Lane::Control), PacketSendResult::Backpressured);
    let sent = fixture.native.sent(0);
    assert_eq!(sent.len(), 1);
    let packet = parse_terminal_peer_packet(Lane::Control, &sent[0]).unwrap();
    assert_eq!((packet.header.message_id, packet.header.total_bytes, packet.payload), (1, 3, &[7u8, 8, 9][..]));
    fixture.emit_low(Lane::Control);
    assert_eq!(fixture.native.sent(0).len(), 1);
    assert_eq!(fixture.port.send(vec![1], Lane::Control), PacketSendResult::Accepted);
    assert_eq!(fixture.native.sent(0).len(), 2);
}

#[tokio::test]
async fn reassembles_framed_control_rejects_malformed_packets_and_releases_a_partial_quota_on_close() {
    let fixture = fixture();
    fixture.emit_message(Lane::Control, framed_control(1, &[4, 5]));
    assert_eq!(fixture.messages(), vec![vec![4, 5]]);
    let unauthenticated = self::fixture();
    unauthenticated.emit_message(Lane::Control, framed_control(1, &[0; 4_097]));
    assert!(!unauthenticated.port.is_open());
    assert_eq!(unauthenticated.close_reasons(), vec!["unauthenticated_frame_too_large"]);

    fixture.port.mark_authenticated();
    let total = TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES + 1;
    let header = TerminalPeerPacketHeader { message_id: 2, total_bytes: total as u32, offset_bytes: 0 };
    let first = encode_terminal_peer_packet(Lane::Control, header, &vec![3; TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES]).unwrap();
    fixture.emit_message(Lane::Control, first);
    assert_eq!(fixture.budget.snapshot(PacketDirection::Incoming).control_bytes, total);
    fixture.port.close(1000, "test");
    assert_eq!(fixture.budget.snapshot(PacketDirection::Incoming).retained_bytes, 0);

    let healthy = self::fixture();
    let mut malformed = framed_control(1, &[1]);
    malformed[0..4].copy_from_slice(&0u32.to_le_bytes());
    healthy.emit_message(Lane::Control, malformed);
    assert!(!healthy.port.is_open());
    assert_eq!(healthy.close_reasons(), vec!["packet_rejected"]);
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
    let mut quotas: Vec<_> = (0..3).map(|_| worker.create_peer_budget().unwrap().quota(PacketDirection::Outgoing, Lane::History)).collect();
    assert!(quotas[0].reserve(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES));
    assert!(quotas[1].reserve(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES));
    assert!(!quotas[2].reserve(1), "the worker's application ceiling refuses a third peer");
    quotas[0].release(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES);
    quotas[1].release(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES);

    let fixture = fixture();
    for lane in Lane::ALL {
        fixture.saturate(lane);
    }
    assert_eq!(fixture.port.send(vec![1], Lane::History), PacketSendResult::Backpressured);
    assert_eq!(fixture.port.send(vec![2], Lane::Terminal), PacketSendResult::Backpressured);
    assert_eq!(fixture.port.send(vec![3], Lane::Control), PacketSendResult::Backpressured);
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
    assert_eq!(fixture.port.send(vec![9], Lane::History), PacketSendResult::Backpressured);
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
    let mut reservation = fixture.port.reserve_history_read(reserved).expect("history pre-read reservation");
    let outgoing = |fixture: &Fixture| fixture.budget.snapshot(PacketDirection::Outgoing).application_bytes;
    assert_eq!(outgoing(&fixture), reserved);
    assert_eq!(fixture.port.send(vec![7], Lane::History), PacketSendResult::Backpressured);
    assert_eq!(outgoing(&fixture), reserved + 1);
    reservation.transfer();
    assert_eq!(fixture.port.send(vec![9], Lane::History), PacketSendResult::Backpressured);
    assert_eq!(outgoing(&fixture), 2);
    drop(reservation);
    assert_eq!(outgoing(&fixture), 2);
    fixture.emit_low(Lane::History);
    fixture.port.wait_for_lane_drain(Lane::History).await;
    assert_eq!(outgoing(&fixture), 0);

    let orphan = self::fixture();
    let reservation = orphan.port.reserve_history_read(reserved).expect("orphan history reservation");
    orphan.port.close(1000, "test");
    assert_eq!(outgoing(&orphan), reserved, "a read in flight keeps its bytes charged past close");
    drop(reservation);
    assert_eq!(outgoing(&orphan), 0);
}

#[tokio::test]
async fn retires_a_history_holder_instead_of_the_healthy_terminal_sender_under_worker_pressure() {
    let worker = TerminalPeerPacketBudget::new();
    let first = fixture_with(worker.clone());
    let second = fixture_with(worker.clone());
    let healthy = fixture_with(worker);
    let first_reservation = first.port.reserve_history_read(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES).expect("first");
    let second_reservation = second.port.reserve_history_read(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES).expect("second");

    assert_eq!(healthy.port.send(vec![0; 16 * 1024], Lane::Terminal), PacketSendResult::Accepted);
    assert!(healthy.port.is_open());
    assert!(!first.port.is_open());
    assert!(first.close_reasons().contains(&"application_pressure".to_owned()));
    assert!(second.port.is_open());

    drop(first_reservation);
    drop(second_reservation);
    second.port.close(1000, "test");
    healthy.port.close(1000, "test");
}

#[tokio::test]
async fn refuses_non_control_client_data_before_it_can_reach_a_terminal_ingress() {
    let fixture = fixture();
    fixture.emit_message(Lane::Terminal, framed_control(1, &[1]));
    assert!(!fixture.port.is_open());
    assert!(fixture.messages().is_empty());
    assert_eq!(fixture.close_reasons(), vec!["unexpected_client_data"]);
}

/// Budgets refuse an over-limit packet: a control frame beyond the peer's
/// control ceiling is refused at admission, not queued.
#[tokio::test]
async fn a_send_beyond_the_peer_control_ceiling_is_refused() {
    let fixture = fixture();
    fixture.saturate(Lane::Control);
    let frame = 128 * 1024;
    assert_eq!(fixture.port.send(vec![0; frame], Lane::Control), PacketSendResult::Backpressured);
    assert_eq!(fixture.port.send(vec![0; frame], Lane::Control), PacketSendResult::Backpressured);
    assert_eq!(fixture.port.send(vec![0; 1], Lane::Control), PacketSendResult::Refused);
    assert_eq!(fixture.budget.snapshot(PacketDirection::Outgoing).control_bytes, TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES);
}
