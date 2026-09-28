//! Direct-carrier Hello rules. A peer port must repeat its exact offer tuple
//! before the socket owner marks native framing authenticated, and a peer's
//! input must name its live route epoch; loopback keeps its compatibility path
//! only when `peer_id` and `worker_epoch` are absent. Ports
//! `apps/worker/tests/local-door/local-terminal-peer-socket.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod local_terminal_support;
mod terminal_stream_support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use local_terminal_support::{
    DEVICE, Fixture, GRANT_ID, SECRET, TAB, WORKER_EPOCH, closed_reason, encode, input, settle,
};
use roost_proto::__buffa::oneof::local_terminal_client_frame::Frame as ClientFrame;
use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_proto::LocalTerminalHello;
use roost_proto::buffa::Message;
use roost_protocol::terminal_peer::peer::TerminalPeerPacketLane;
use roost_worker::local_terminal::{
    ExpectedPeer, HistoryReadReservation, PacketSendResult, PeerIngress, PeerTerminalPacketPort,
    TerminalPacketPort,
};
use roost_worker::uplink::OwnerFuture;
use terminal_stream_support::{SESSION, held};

const PEER_ID: &str = "44444444-4444-4444-8444-444444444444";
const PEER_SOCKET: &str = "55555555-5555-4555-8555-555555555555";

/// A WebRTC port as the peer owner presents one.
#[derive(Debug, Default)]
struct PeerPort {
    frames: Mutex<Vec<ServerFrame>>,
    authenticated: AtomicBool,
    closed: AtomicBool,
    ingress: Mutex<Option<Arc<PeerIngress>>>,
}

impl TerminalPacketPort for PeerPort {
    fn socket_id(&self) -> &str {
        PEER_SOCKET
    }
    fn is_open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }
    fn send(&self, bytes: Vec<u8>, _lane: TerminalPeerPacketLane) -> PacketSendResult {
        let frame = roost_proto::LocalTerminalServerFrame::decode_from_slice(&bytes).unwrap();
        held(&self.frames).push(frame.frame.unwrap());
        PacketSendResult::Accepted
    }
    fn close(&self, _code: u16, _reason: &str) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let ingress = held(&self.ingress).clone();
            if let Some(ingress) = ingress {
                ingress.on_close();
            }
        }
    }
}

impl PeerTerminalPacketPort for PeerPort {
    fn mark_authenticated(&self) {
        self.authenticated.store(true, Ordering::SeqCst);
    }
    fn reserve_history_read(&self, _bytes: usize) -> Option<Box<dyn HistoryReadReservation>> {
        None
    }
    fn wait_for_lane_drain(&self, _lane: TerminalPeerPacketLane) -> OwnerFuture<()> {
        Box::pin(std::future::ready(()))
    }
}

fn peer_hello(peer_id: &str, worker_epoch: &str) -> ClientFrame {
    ClientFrame::from(LocalTerminalHello {
        grant_id: GRANT_ID.to_owned(),
        secret: SECRET.to_owned(),
        tab_id: TAB.to_owned(),
        device_fingerprint: DEVICE.to_owned(),
        peer_id: peer_id.to_owned(),
        worker_epoch: worker_epoch.to_owned(),
        ..Default::default()
    })
}

#[tokio::test]
async fn peer_hello_matches_the_offer_tuple_before_it_becomes_authenticated() {
    let fixture = Fixture::new();
    let expected = ExpectedPeer {
        peer_id: PEER_ID.to_owned(),
        grant_id: GRANT_ID.to_owned(),
        device_fingerprint: DEVICE.to_owned(),
        tab_id: TAB.to_owned(),
        worker_epoch: WORKER_EPOCH.to_owned(),
    };
    let port = Arc::new(PeerPort::default());
    let ingress = Arc::new(fixture.sockets.open_peer_port(
        Arc::clone(&port) as Arc<dyn PeerTerminalPacketPort>,
        expected,
    ));
    *held(&port.ingress) = Some(Arc::clone(&ingress));

    ingress.on_message(&encode(peer_hello(PEER_ID, WORKER_EPOCH)));

    assert!(port.authenticated.load(Ordering::SeqCst));
    let ServerFrame::Ready(ready) = held(&port.frames)[0].clone() else {
        panic!("a ready frame")
    };
    assert_eq!(
        (
            ready.worker_epoch.as_str(),
            ready.socket_id.as_str(),
            ready.peer_id.as_str()
        ),
        (WORKER_EPOCH, PEER_SOCKET, PEER_ID)
    );

    // A peer names its route epoch; an epoch-less batch is a changed route.
    ingress.on_message(&encode(input(SESSION, 1, b"x")));
    settle().await;
    let last = held(&port.frames).last().cloned();
    let Some(ServerFrame::InputRejected(rejected)) = last else {
        panic!("a rejection")
    };
    assert_eq!(rejected.reason, "terminal input route changed");
}

#[tokio::test]
async fn loopback_rejects_a_peer_shaped_hello_instead_of_accepting_old_compatibility() {
    let fixture = Fixture::new();
    let stub = fixture.open();

    fixture.send(&stub, peer_hello(PEER_ID, WORKER_EPOCH));

    assert!(!stub.is_open());
    assert_eq!(stub.cases(), ["closed"]);
    assert_eq!(
        closed_reason(&stub.frames()[0]),
        "loopback hello must not include peer identity"
    );
}
