//! A terminal peer packet port over three fake native channels, its ingress
//! recorded, and v2's channel helpers (`emitLow`, saturation, framed
//! control). Included by `terminal_peer_packet_port.rs` and
//! `terminal_peer_packet_ingress.rs`. Ports the fixture of v2
//! `apps/worker/tests/terminal/peer/terminal-peer-packet-port.test.ts`.
#![allow(dead_code)]

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_protocol::terminal_peer::packets::{
    TerminalPeerPacketHeader, TerminalPeerPacketLane as Lane, encode_terminal_peer_packet,
};
use roost_protocol::terminal_peer::peer::TerminalPeerChannelWatermarks;
use roost_worker::peer::native::NativePeerEvent;
use roost_worker::peer::{
    PacketPortDeps, PeerTestFaults, TerminalPeerPacketBudget, TerminalPeerPacketIngress,
    TerminalPeerPacketPort,
};

use super::fake_native::FakePeer;

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
pub struct Recorded {
    pub messages: Mutex<Vec<Vec<u8>>>,
    pub close_reasons: Mutex<Vec<String>>,
}

pub struct RecordingIngress(pub Arc<Recorded>);

impl TerminalPeerPacketIngress for RecordingIngress {
    fn on_message(&self, bytes: &[u8]) {
        lock(&self.0.messages).push(bytes.to_vec());
    }
    fn on_close(&self) {}
}

pub struct Fixture {
    pub budget: TerminalPeerPacketBudget,
    pub native: Arc<FakePeer>,
    pub port: Arc<TerminalPeerPacketPort>,
    pub recorded: Arc<Recorded>,
}

impl Fixture {
    pub fn messages(&self) -> Vec<Vec<u8>> {
        lock(&self.recorded.messages).clone()
    }
    pub fn close_reasons(&self) -> Vec<String> {
        lock(&self.recorded.close_reasons).clone()
    }
    pub fn emit(&self, event: NativePeerEvent) {
        self.port.on_channel_event(event);
    }
    /// v2 `channel.emitLow()`.
    pub fn emit_low(&self, lane: Lane) {
        self.native.set_buffered(lane as usize, 0);
        self.emit(NativePeerEvent::BufferedAmountLow(lane as usize));
    }
    pub fn emit_message(&self, lane: Lane, data: Vec<u8>) {
        self.emit(NativePeerEvent::ChannelMessage {
            channel: lane as usize,
            binary: true,
            data,
        });
    }
    pub fn saturate(&self, lane: Lane) {
        self.native.set_buffered(
            lane as usize,
            TerminalPeerChannelWatermarks::for_lane(lane).0,
        );
    }
}

pub fn fixture_with(budget: TerminalPeerPacketBudget) -> Fixture {
    fixture_built(budget, None)
}

/// A port that reads the smoke harness's peer faults.
pub fn fixture_with_faults(faults: Arc<PeerTestFaults>) -> Fixture {
    fixture_built(TerminalPeerPacketBudget::new(), Some(faults))
}

fn fixture_built(
    budget: TerminalPeerPacketBudget,
    test_faults: Option<Arc<PeerTestFaults>>,
) -> Fixture {
    let native = FakePeer::standalone(3);
    let recorded = Arc::new(Recorded::default());
    let closes = Arc::clone(&recorded);
    let port = TerminalPeerPacketPort::new(PacketPortDeps {
        socket_id: "peer-socket".into(),
        native: Arc::clone(&native) as _,
        budget: budget.clone(),
        peer_budget: budget.create_peer_budget().unwrap(),
        on_closed: Some(Arc::new(move |reason: &str| {
            lock(&closes.close_reasons).push(reason.to_owned())
        })),
        on_fatal: None,
        test_faults,
        runtime: tokio::runtime::Handle::current(),
    });
    assert!(port.attach_ingress(Arc::new(RecordingIngress(Arc::clone(&recorded)))));
    for channel in 0..3 {
        native.set_open(channel, true);
        port.on_channel_event(NativePeerEvent::ChannelOpen(channel));
    }
    Fixture {
        budget,
        native,
        port,
        recorded,
    }
}

pub fn fixture() -> Fixture {
    fixture_with(TerminalPeerPacketBudget::new())
}

pub fn framed_control(message_id: u32, payload: &[u8]) -> Vec<u8> {
    let header = TerminalPeerPacketHeader {
        message_id,
        total_bytes: payload.len() as u32,
        offset_bytes: 0,
    };
    encode_terminal_peer_packet(Lane::Control, header, payload).unwrap()
}
