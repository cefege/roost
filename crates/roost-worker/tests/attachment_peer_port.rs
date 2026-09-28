//! The attachment peer packet port over the shared fake native: fragments are
//! reassembled per channel before the direct receiver sees them, the data
//! channel is closed to a peer that has not been admitted, and an unadmitted
//! peer is retired at the hello deadline. Pins the guards of v2
//! `apps/worker/src/attachments/attachment-peer-packet-port.ts` (`receive`,
//! the control-channel `onOpen` hello timer) that no v2 test names.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "peer_support/fake_native.rs"]
mod fake_native;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fake_native::{FakeNative, FakePeer, OFFER_FINGERPRINT, offer_sdp};
use roost_protocol::attachment_transfer::{
    AttachmentTransferPacketHeader, HELLO_DEADLINE_MS, PACKET_MAX_PAYLOAD_BYTES, PeerChannelLane,
    encode_attachment_transfer_packet,
};
use roost_worker::attachments::peer_budget::AttachmentPeerPacketBudget;
use roost_worker::attachments::peer_connection::{
    AttachmentPeerConnection, AttachmentPeerConnectionConfig, AttachmentPeerConnectionDeps,
};
use roost_worker::attachments::peer_packet_port::{
    AttachmentPeerIngress, AttachmentPeerPacketPort,
};
use roost_worker::attachments::transfer_admission::AttachmentPeerExpectedTuple;
use roost_worker::attachments::transfer_port::AttachmentTransferPort;
use roost_worker::peer::native::NativePeerEvent;

/// What the direct receiver would have been handed.
#[derive(Debug, Default)]
struct Received {
    frames: Mutex<Vec<(PeerChannelLane, Vec<u8>)>>,
    closed: Mutex<bool>,
}

#[derive(Debug)]
struct RecordingIngress(Arc<Received>);

impl AttachmentPeerIngress for RecordingIngress {
    fn on_message(&self, lane: PeerChannelLane, bytes: Vec<u8>) {
        self.0.frames.lock().unwrap().push((lane, bytes));
    }

    fn on_close(&self) {
        *self.0.closed.lock().unwrap() = true;
    }
}

struct Harness {
    peer: Arc<FakePeer>,
    port: Arc<AttachmentPeerPacketPort>,
    received: Arc<Received>,
    _connection: AttachmentPeerConnection,
}

async fn harness() -> Harness {
    let fake = FakeNative::new();
    let factory = (fake.loader())().await.unwrap();
    let received = Arc::new(Received::default());
    let opened: Arc<Mutex<Option<Arc<AttachmentPeerPacketPort>>>> = Arc::default();
    let (sink, record) = (Arc::clone(&received), Arc::clone(&opened));
    let connection =
        AttachmentPeerConnection::new(AttachmentPeerConnectionDeps {
            factory,
            peer_id: "11111111-1111-4111-8111-111111111111".to_owned(),
            expected_tuple: AttachmentPeerExpectedTuple {
                peer_id: "11111111-1111-4111-8111-111111111111".to_owned(),
                grant_id: "grant".to_owned(),
                device_fingerprint: "c".repeat(64),
                tab_id: "tab".to_owned(),
                worker_epoch: "epoch".to_owned(),
            },
            expected_remote_fingerprint: OFFER_FINGERPRINT.to_owned(),
            config: AttachmentPeerConnectionConfig::default(),
            packet_budget: AttachmentPeerPacketBudget::new().create_peer_budget(),
            open_peer_port: Arc::new(
                move |port: Arc<AttachmentPeerPacketPort>, _: AttachmentPeerExpectedTuple| {
                    *record.lock().unwrap() = Some(port);
                    Some(Arc::new(RecordingIngress(Arc::clone(&sink)))
                        as Arc<dyn AttachmentPeerIngress>)
                },
            ),
            on_closed: Box::new(|_| {}),
        })
        .unwrap();
    connection
        .answer(offer_sdp(), Duration::from_secs(8))
        .await
        .unwrap();
    let port = opened.lock().unwrap().clone().unwrap();
    Harness {
        peer: fake.peers()[0].clone(),
        port,
        received,
        _connection: connection,
    }
}

fn packet(message_id: u32, total: usize, offset: usize, payload: &[u8]) -> Vec<u8> {
    let header = AttachmentTransferPacketHeader {
        message_id,
        total_bytes: u32::try_from(total).unwrap(),
        offset_bytes: u32::try_from(offset).unwrap(),
    };
    encode_attachment_transfer_packet(header, payload).unwrap()
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn a_fragmented_control_frame_reaches_the_receiver_whole() {
    let harness = harness().await;
    harness.peer.open_channel(0);
    let frame: Vec<u8> = (0..PACKET_MAX_PAYLOAD_BYTES + 3)
        .map(|index| (index % 7) as u8)
        .collect();
    let (head, tail) = frame.split_at(PACKET_MAX_PAYLOAD_BYTES);
    for fragment in [
        packet(1, frame.len(), 0, head),
        packet(1, frame.len(), head.len(), tail),
    ] {
        harness.peer.emit(NativePeerEvent::ChannelMessage {
            channel: 0,
            binary: true,
            data: fragment,
        });
    }
    settle().await;
    let frames = harness.received.frames.lock().unwrap().clone();
    assert_eq!(frames, vec![(PeerChannelLane::Control, frame)]);
}

#[tokio::test]
async fn the_data_channel_is_closed_to_a_peer_that_was_never_admitted() {
    let harness = harness().await;
    harness.peer.open_channel(0);
    harness.peer.open_channel(1);
    harness.peer.emit(NativePeerEvent::ChannelMessage {
        channel: 1,
        binary: true,
        data: packet(1, 1, 0, &[1]),
    });
    settle().await;
    assert!(harness.received.frames.lock().unwrap().is_empty());
    assert!(*harness.received.closed.lock().unwrap());
    assert!(!harness.port.is_open());
    assert!(harness.peer.is_closed());
}

#[tokio::test]
async fn an_admitted_peer_writes_the_data_channel() {
    let harness = harness().await;
    harness.peer.open_channel(0);
    harness.peer.open_channel(1);
    harness.port.mark_authenticated();
    harness.peer.emit(NativePeerEvent::ChannelMessage {
        channel: 1,
        binary: true,
        data: packet(1, 2, 0, &[4, 2]),
    });
    settle().await;
    let frames = harness.received.frames.lock().unwrap().clone();
    assert_eq!(frames, vec![(PeerChannelLane::Data, vec![4, 2])]);
}

#[tokio::test(start_paused = true)]
async fn an_unadmitted_peer_is_retired_at_the_hello_deadline() {
    let harness = harness().await;
    harness.peer.open_channel(0);
    settle().await;
    tokio::time::sleep(Duration::from_millis(HELLO_DEADLINE_MS - 1)).await;
    assert!(harness.port.is_open());
    tokio::time::sleep(Duration::from_millis(2)).await;
    settle().await;
    assert!(!harness.port.is_open());
    assert!(*harness.received.closed.lock().unwrap());
}
