//! A direct upload over a real in-process str0m pair lands a file: the worker's
//! attachment peer owner answers a browser-side str0m offerer, the hello and
//! chunk travel packet-framed on their own channels, and the final ack names
//! the committed path. No v2 test drives both natives in one process; this is
//! the Rust acceptance proof for `attachment-peer-*` over the shared driver.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "peer_support/offerer.rs"]
mod offerer;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use offerer::BrowserOfferer;
use roost_proto::__buffa::oneof::attachment_transfer_client_frame::Frame as ClientFrame;
use roost_proto::__buffa::oneof::attachment_transfer_server_frame::Frame as ServerFrame;
use roost_proto::buffa::Message;
use roost_proto::{
    AttachmentTransferChunk, AttachmentTransferClientFrame, AttachmentTransferHello,
    AttachmentTransferServerFrame, DLocalAttachmentGrant, DLocalAttachmentPeerOffer,
};
use roost_protocol::attachment_transfer::{
    AttachmentTransferPacketAssembler, AttachmentTransferPacketDirection as Direction,
    AttachmentTransferPacketQueue, AttachmentTransferPacketQuota, PEER_DATA_CHANNELS,
};
use roost_worker::attachments::direct_owners::{AttachmentDirect, AttachmentDirectDeps};
use roost_worker::attachments::grants::AttachmentGrantStore;
use roost_worker::attachments::peer_owner::AttachmentPeerBootstrapState;
use roost_worker::attachments::store_paths::AttachmentBase;
use roost_worker::attachments::system_clock;
use roost_worker::attachments::upload::AttachmentOperations;
use roost_worker::link_ports::AttachmentPeerPort;
use roost_worker::peer::PeerTransportConfig;
use roost_worker::peer::coordinator_generation::CoordinatorGeneration;
use roost_worker::peer::native::{NativeChannelSpec, NativePeerEvent, str0m_loader};
use roost_worker::session::ids::mint_uuid;
use roost_worker::uplink::{RequestBudget, Uplink};
use sha2::{Digest, Sha256};

const DEVICE: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

struct Unbounded;

impl AttachmentTransferPacketQuota for Unbounded {
    fn reserve(&mut self, _direction: Direction, _bytes: usize) -> bool {
        true
    }

    fn release(&mut self, _direction: Direction, _bytes: usize) {}
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn channel_specs() -> Vec<NativeChannelSpec> {
    PEER_DATA_CHANNELS
        .iter()
        .map(|channel| NativeChannelSpec {
            id: channel.id,
            label: channel.label.to_owned(),
            ordered: channel.ordered,
            protocol: channel.protocol.to_owned(),
        })
        .collect()
}

/// The browser side of one channel: frames go out packet-framed.
fn send_framed(offerer: &BrowserOfferer, queue: &mut AttachmentTransferPacketQueue<Unbounded>, channel: usize, frame: ClientFrame) {
    let bytes = AttachmentTransferClientFrame { frame: Some(frame), ..Default::default() }.encode_to_vec();
    assert!(queue.enqueue(bytes).unwrap());
    while let Some(fragment) = queue.next_fragment().unwrap() {
        offerer.send(channel, fragment.bytes());
        fragment.commit();
    }
}

/// The next whole server frame on the control channel.
async fn next_server_frame(offerer: &mut BrowserOfferer, control: &mut AttachmentTransferPacketAssembler<Unbounded>, origin: Instant) -> ServerFrame {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), offerer.next_event()).await.expect("the worker answers within the deadline");
        if let NativePeerEvent::ChannelMessage { channel: 0, data, .. } = event {
            let now_ms = u64::try_from(origin.elapsed().as_millis()).unwrap();
            if let Some(message) = control.push(&data, now_ms).unwrap() {
                return AttachmentTransferServerFrame::decode_from_slice(&message).unwrap().frame.unwrap();
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_direct_upload_over_an_in_process_str0m_pair_lands_the_file() {
    let root = std::env::temp_dir().join(format!("roost-peer-upload-{}", mint_uuid().unwrap()));
    let worker_epoch = mint_uuid().unwrap();
    let grants = Arc::new(AttachmentGrantStore::system(worker_epoch.clone()));
    let operations = AttachmentOperations::new(AttachmentBase::new(root.clone()), system_clock());
    let generation = CoordinatorGeneration::default();
    let direct = AttachmentDirect::new(AttachmentDirectDeps {
        grants: Arc::clone(&grants),
        operations,
        worker_fingerprint: "b".repeat(64),
        worker_epoch: worker_epoch.clone(),
        peer: PeerTransportConfig { enabled: true, bind_address: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)), port_range: None },
        native_loader: str0m_loader(),
        coordinator_generation: generation,
    });
    assert_eq!(direct.bootstrap().await, AttachmentPeerBootstrapState::Ready);

    let (secret, payload) = (mint_uuid().unwrap(), b"attachment over a str0m pair".to_vec());
    let (session_id, upload_id, grant_id, tab_id, peer_id) =
        (format!("peer-{}", mint_uuid().unwrap()), mint_uuid().unwrap(), mint_uuid().unwrap(), mint_uuid().unwrap(), mint_uuid().unwrap());
    grants
        .install(&DLocalAttachmentGrant {
            request_id: mint_uuid().unwrap(),
            grant_id: grant_id.clone(),
            secret_sha256: hex(&Sha256::digest(secret.as_bytes())),
            session_id: session_id.clone(),
            upload_id: upload_id.clone(),
            filename: "peer.bin".to_owned(),
            short_path: false,
            total_bytes: payload.len() as u64,
            device_fingerprint: DEVICE.to_owned(),
            tab_id: tab_id.clone(),
            ttl_ms: 60_000,
            worker_epoch: worker_epoch.clone(),
            ..Default::default()
        })
        .unwrap();

    let (mut offerer, offer_sdp) = BrowserOfferer::start(&channel_specs()).await;
    let offer = DLocalAttachmentPeerOffer {
        request_id: mint_uuid().unwrap(),
        connection_generation: mint_uuid().unwrap(),
        worker_epoch: worker_epoch.clone(),
        grant_id: grant_id.clone(),
        peer_id: peer_id.clone(),
        device_fingerprint: DEVICE.to_owned(),
        tab_id: tab_id.clone(),
        offer_sdp,
        budget_ms: 8_000,
        ..Default::default()
    };
    let budget = RequestBudget::from_budget_ms(8_000, Instant::now());
    let answer = direct.offer(offer, budget, Uplink::detached().fence()).await.expect("the worker answers the offer");
    assert_eq!(answer.peer_id, peer_id);
    offerer.accept_answer(&answer.answer_sdp);

    let origin = Instant::now();
    let mut control_out = AttachmentTransferPacketQueue::new(Direction::Outgoing, Unbounded);
    let mut data_out = AttachmentTransferPacketQueue::new(Direction::Outgoing, Unbounded);
    let mut control_in = AttachmentTransferPacketAssembler::new(Direction::Incoming, Unbounded);
    offerer.wait_channels_open(2).await;
    send_framed(&offerer, &mut control_out, 0, ClientFrame::Hello(Box::new(AttachmentTransferHello {
        grant_id, secret, tab_id, device_fingerprint: DEVICE.to_owned(), session_id: session_id.clone(), upload_id: upload_id.clone(),
        filename: "peer.bin".to_owned(), short_path: false, total_bytes: payload.len() as u64, peer_id, worker_epoch, ..Default::default()
    })));
    let ServerFrame::Ready(ready) = next_server_frame(&mut offerer, &mut control_in, origin).await else {
        panic!("the worker admits the hello with a ready frame");
    };
    assert_eq!((ready.session_id.as_str(), ready.upload_id.as_str()), (session_id.as_str(), upload_id.as_str()));

    let digest = hex(&Sha256::digest(&payload));
    send_framed(&offerer, &mut data_out, 1, ClientFrame::Chunk(Box::new(AttachmentTransferChunk {
        upload_id: upload_id.clone(), seq: 0, offset: 0, data: payload.clone(), last: true, chunk_sha256: digest.clone(), ..Default::default()
    })));
    let ServerFrame::Ack(ack) = next_server_frame(&mut offerer, &mut control_in, origin).await else {
        panic!("the final chunk is acknowledged");
    };
    assert_eq!((ack.error.as_str(), ack.chunk_sha256.as_str(), ack.bytes_received), ("", digest.as_str(), payload.len() as u64));
    assert_eq!(std::fs::read(&ack.abs_path).unwrap(), payload);
    let ServerFrame::Closed(closed) = next_server_frame(&mut offerer, &mut control_in, origin).await else {
        panic!("the carrier closes after the final ack");
    };
    assert_eq!(closed.reason, "complete");
    let _removed = std::fs::remove_dir_all(&root);
}
