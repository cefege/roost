//! The two direct carriers, end to end over fake transports.
//!
//! These would need a browser if the carriers owned their sockets. They do not:
//! a host opens what these modules name, writes what they hand it, and reports
//! what came back, so the whole conversation — hello, Ready, chunks,
//! acknowledgements, a status request — runs here over real protobuf frames.
//! The packet framing underneath the peer route is in `attachment_packets.rs`.
//!
//! The v2 names are kept verbatim. The mutation experiment, in the slice report:
//! drop the `worker_epoch` comparison from `AttachmentConversation::admit_ready`
//! and a Ready naming another epoch slips through.

use roost_client_core::client::attachments::conversation::ConversationOutcome;
use roost_client_core::client::attachments::direct::LocalWorkerDoor;
use roost_client_core::client::attachments::direct::loopback::{
    LOOPBACK_PATH, LOOPBACK_SUBPROTOCOL, LoopbackTransfer,
};
use roost_client_core::client::attachments::grant::{
    AttachmentDirectGrant, AttachmentDirectGrantRequest, AttachmentDirectGrantResponse,
};
use roost_client_core::client::attachments::packets::assembler::AttachmentPacketAssembler;
use roost_client_core::client::attachments::packets::{
    ATTACHMENT_PACKET_MAX_BYTES, AttachmentPacketHeader, PeerLane, encode_attachment_packet,
};
use roost_client_core::client::attachments::peer::AttachmentPeerTransfer;
use roost_client_core::client::attachments::transfer::receipt::AttachmentTransferStatus;
use roost_client_core::client::attachments::transfer::{
    AttachmentTransferAck, DIRECT_CHUNK_BYTES, DirectUpload, InFlightChunk, SliceRequest,
};
use roost_proto::__buffa::oneof::attachment_transfer_client_frame::Frame as ClientFrame;
use roost_proto::__buffa::oneof::attachment_transfer_server_frame::Frame as ServerFrame;
use roost_proto::buffa::Message;
use roost_proto::{
    AttachmentTransferAck as ProtoAck, AttachmentTransferClientFrame, AttachmentTransferReady,
    AttachmentTransferServerFrame, AttachmentTransferStatus as ProtoStatus,
};

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The committed path the fake worker reports.
const WORKER_PATH: &str = "/worker/peer.bin";

/// The grant these tests' carriers authenticate with.
fn grant(total_bytes: u64) -> AttachmentDirectGrant {
    let request = AttachmentDirectGrantRequest {
        worker_fp: "worker-a".to_owned(),
        session_id: "session-a".to_owned(),
        upload_id: "upload-a".to_owned(),
        filename: "peer.bin".to_owned(),
        short_path: false,
        total_bytes,
    };
    let response = AttachmentDirectGrantResponse {
        grant_id: "grant-a".to_owned(),
        secret: "secret-a".to_owned(),
        worker_epoch: "epoch-a".to_owned(),
        peer_supported: true,
        stun_urls: Vec::new(),
    };
    AttachmentDirectGrant::from_response(request, "tab-a", "device-a", Ok(response))
        .expect("a complete answer is a grant")
}

/// The first chunk of a `total_bytes` upload, as a real transfer frames it.
fn first_chunk(total_bytes: u64) -> (DirectUpload, InFlightChunk) {
    let mut upload = DirectUpload::new("upload-a", total_bytes);
    let request = upload.next_slice().expect("an upload has a first slice");
    upload
        .begin_chunk(vec![request.bytes as u8; request.bytes], DIGEST)
        .expect("the first slice is frameable");
    let in_flight = upload.in_flight().expect("a chunk is in flight").clone();
    (upload, in_flight)
}

/// The worker's Ready, as the bytes a socket delivers.
fn ready_frame_bytes() -> Vec<u8> {
    server_frame_bytes(ready_frame())
}

/// Wrap a server frame in the one packet an idle channel would carry.
fn server_packet(message_id: u32, frame: ServerFrame) -> Vec<u8> {
    let bytes = server_frame_bytes(frame);
    let header = AttachmentPacketHeader {
        message_id,
        total_bytes: bytes.len() as u32,
        offset_bytes: 0,
    };
    encode_attachment_packet(header, &bytes).expect("a frame this size is encodable")
}

/// A worker's Ready for the tuple both carriers authenticate.
fn ready_frame() -> ServerFrame {
    ServerFrame::Ready(Box::new(AttachmentTransferReady {
        __buffa_unknown_fields: Default::default(),
        worker_fingerprint: "worker-a".to_owned(),
        worker_epoch: "epoch-a".to_owned(),
        session_id: "session-a".to_owned(),
        upload_id: "upload-a".to_owned(),
    }))
}

/// Any server frame, as the bytes a socket delivers.
fn server_frame_bytes(frame: ServerFrame) -> Vec<u8> {
    AttachmentTransferServerFrame {
        __buffa_unknown_fields: Default::default(),
        frame: Some(frame),
        ..Default::default()
    }
    .encode_to_vec()
}

/// The worker's acknowledgement for one chunk, echoing its own digest.
fn ack_frame(chunk: &InFlightChunk, bytes_received: u64) -> ServerFrame {
    ServerFrame::Ack(Box::new(ProtoAck {
        __buffa_unknown_fields: Default::default(),
        upload_id: chunk.upload_id.clone(),
        seq: chunk.seq,
        bytes_received,
        abs_path: if chunk.last {
            WORKER_PATH.to_owned()
        } else {
            String::new()
        },
        error: String::new(),
        chunk_sha256: chunk.chunk_sha256.clone(),
    }))
}

/// Reassemble whatever a carrier sent on one lane, the way a worker would.
fn decode_client_lane(lane: PeerLane, packets: &[Vec<u8>], now_ms: u64) -> Vec<Vec<u8>> {
    let mut assembler = AttachmentPacketAssembler::new(lane);
    let mut messages = Vec::new();
    for packet in packets {
        if let Some(message) = assembler
            .push(packet, now_ms)
            .expect("a packet this carrier produced is well formed")
        {
            messages.push(message);
        }
    }
    messages
}

/// Drain one flush pass, keeping each lane's packets.
fn flush_once(
    peer: &mut AttachmentPeerTransfer,
    control: &mut Vec<Vec<u8>>,
    data: &mut Vec<Vec<u8>>,
) {
    for packet in peer
        .flush(&[PeerLane::Control, PeerLane::Data])
        .expect("a flush of a well-formed queue succeeds")
    {
        match packet.lane {
            PeerLane::Control => control.push(packet.bytes),
            PeerLane::Data => data.push(packet.bytes),
        }
    }
}

// ----------------------------------------------------------------- the peer

#[test]
fn fragments_attachment_frames_on_separate_ordered_channels_and_completes_from_an_ack() {
    let mut peer = AttachmentPeerTransfer::new(grant(DIRECT_CHUNK_BYTES), "peer-a");

    // The two channels are static, ordered, negotiated in band, and their ids
    // are fixed so the far end cannot choose which lane is control.
    let definitions = peer.channel_definitions();
    assert_eq!(
        definitions
            .iter()
            .map(|definition| (definition.label, definition.id, definition.lane))
            .collect::<Vec<_>>(),
        vec![
            ("roost-attachment-control-v1", 0, PeerLane::Control),
            ("roost-attachment-data-v1", 1, PeerLane::Data),
        ]
    );
    assert_eq!(peer.channel_protocol(), "roost.attachment-transfer.v1");

    let mut control_packets: Vec<Vec<u8>> = Vec::new();
    let mut data_packets: Vec<Vec<u8>> = Vec::new();

    // Opening control authenticates; opening data makes the carrier usable.
    assert_eq!(peer.channel_opened(PeerLane::Control), Ok(None));
    assert_eq!(peer.channel_opened(PeerLane::Data), Ok(None));
    flush_once(&mut peer, &mut control_packets, &mut data_packets);
    assert!(!peer.is_authenticated(), "no Ready has arrived yet");

    // The first thing on control is the hello, naming the whole grant.
    let control_messages = decode_client_lane(PeerLane::Control, &control_packets, 0);
    assert_eq!(control_messages.len(), 1);
    let hello = AttachmentTransferClientFrame::decode_from_slice(&control_messages[0])
        .expect("the hello is a client frame");
    let Some(ClientFrame::Hello(hello)) = hello.frame else {
        panic!("the attachment peer must authenticate before it sends anything");
    };
    assert_eq!(hello.grant_id, "grant-a");
    assert_eq!(hello.secret, "secret-a");
    assert_eq!(
        (hello.tab_id.as_str(), hello.device_fingerprint.as_str()),
        ("tab-a", "device-a")
    );
    assert_eq!(
        (hello.session_id.as_str(), hello.upload_id.as_str()),
        ("session-a", "upload-a")
    );
    assert_eq!(hello.filename, "peer.bin");
    assert!(!hello.short_path);
    assert_eq!(hello.total_bytes, DIRECT_CHUNK_BYTES);
    assert_eq!(
        (hello.peer_id.as_str(), hello.worker_epoch.as_str()),
        ("peer-a", "epoch-a")
    );

    // Ready authenticates the peer, and only Ready does.
    let ready = peer.packet_received(PeerLane::Control, &server_packet(1, ready_frame()), 10);
    assert_eq!(ready, Ok(ConversationOutcome::Ready));
    assert!(peer.is_authenticated());

    // A whole 512 KiB chunk crosses as many bounded packets.
    let (mut upload, chunk) = first_chunk(DIRECT_CHUNK_BYTES);
    peer.send_chunk(&chunk, vec![42u8; chunk.bytes])
        .expect("an authenticated peer sends a chunk");
    assert!(peer.sent_chunk(), "upload bytes are on the wire");
    while peer.has_queued_packets() {
        flush_once(&mut peer, &mut control_packets, &mut data_packets);
    }
    assert!(
        data_packets.len() > 1,
        "a 512 KiB chunk cannot be one packet, or the channel would have to carry it"
    );
    assert!(
        control_packets
            .iter()
            .chain(data_packets.iter())
            .all(|packet| packet.len() <= ATTACHMENT_PACKET_MAX_BYTES),
        "no packet may exceed the cap the channels negotiated"
    );
    let data_messages = decode_client_lane(PeerLane::Data, &data_packets, 20);
    assert_eq!(data_messages.len(), 1, "one chunk is one logical message");
    let sent = AttachmentTransferClientFrame::decode_from_slice(&data_messages[0])
        .expect("the chunk is a client frame");
    let Some(ClientFrame::Chunk(sent)) = sent.frame else {
        panic!("the data lane carries chunks");
    };
    assert_eq!(
        (sent.upload_id.as_str(), sent.seq, sent.offset),
        ("upload-a", 0, 0)
    );
    assert_eq!(sent.data.len(), DIRECT_CHUNK_BYTES as usize);
    assert!(
        sent.last,
        "a whole chunk of a whole file is the final chunk"
    );
    assert_eq!(sent.chunk_sha256, DIGEST);

    // The worker's acknowledgement settles it, and the path is the result.
    let ack = peer.packet_received(
        PeerLane::Control,
        &server_packet(2, ack_frame(&chunk, DIRECT_CHUNK_BYTES)),
        30,
    );
    let Ok(ConversationOutcome::Ack(receipt)) = ack else {
        panic!("the worker's acknowledgement settles the chunk in flight");
    };
    assert_eq!(receipt.bytes_received, DIRECT_CHUNK_BYTES);
    assert_eq!(receipt.abs_path, WORKER_PATH);
    let settled = upload.settle(&receipt).expect("a matching receipt settles");
    assert!(settled.completed);
    assert_eq!(
        upload.outcome().map(|result| result.abs_path).as_deref(),
        Some(WORKER_PATH)
    );
}

// ------------------------------------------------------------- the loopback

#[test]
fn authenticates_then_sends_direct_bytes_in_order_and_advances_progress_from_acks() {
    let door = LocalWorkerDoor {
        origin: "http://127.0.0.1:4104".to_owned(),
        worker_fingerprint: "worker-a".to_owned(),
    };
    let total = DIRECT_CHUNK_BYTES + 5;
    let mut carrier = LoopbackTransfer::new(&door.worker_fingerprint, grant(total));

    assert_eq!(
        door.loopback_url(),
        format!("ws://127.0.0.1:4104{LOOPBACK_PATH}")
    );
    assert_eq!(carrier.subprotocol(), LOOPBACK_SUBPROTOCOL);

    let hello_bytes = carrier.socket_opened().expect("the socket authenticates");
    let hello = AttachmentTransferClientFrame::decode_from_slice(&hello_bytes)
        .expect("the hello is a client frame");
    let Some(ClientFrame::Hello(hello)) = hello.frame else {
        panic!("the loopback carrier authenticates with a hello");
    };
    assert_eq!(hello.grant_id, "grant-a");
    assert_eq!(hello.secret, "secret-a");
    assert_eq!(
        (hello.tab_id.as_str(), hello.device_fingerprint.as_str()),
        ("tab-a", "device-a")
    );
    assert_eq!(
        (hello.session_id.as_str(), hello.upload_id.as_str()),
        ("session-a", "upload-a")
    );
    assert_eq!(hello.filename, "peer.bin");
    assert!(!hello.short_path);
    assert_eq!(hello.total_bytes, total);
    assert_eq!(hello.worker_epoch, "epoch-a");
    assert!(
        hello.peer_id.is_empty(),
        "loopback authenticates with an empty peer id, which is how the worker \
         tells the two routes apart"
    );

    assert_eq!(
        carrier.frame_received(&ready_frame_bytes()),
        Ok(ConversationOutcome::Ready)
    );
    assert!(carrier.is_ready());

    let file: Vec<u8> = (0..total)
        .map(|index| ((index * 17 + 9) & 0xff) as u8)
        .collect();
    let mut upload = DirectUpload::new("upload-a", total);
    let mut progress = Vec::new();
    let mut sent: Vec<(u32, u64, bool, Vec<u8>)> = Vec::new();
    while let Some(slice) = upload.next_slice() {
        let data = file[slice.offset as usize..slice.offset as usize + slice.bytes].to_vec();
        upload
            .begin_chunk(data, DIGEST)
            .expect("the slice is the one that was asked for");
        let in_flight = upload.in_flight().expect("a chunk is in flight").clone();
        let frame_bytes = carrier
            .send_chunk(&in_flight, slice_bytes(&slice, &file))
            .expect("a ready carrier sends a chunk");
        let frame = AttachmentTransferClientFrame::decode_from_slice(&frame_bytes)
            .expect("the chunk is a client frame");
        let Some(ClientFrame::Chunk(frame)) = frame.frame else {
            panic!("the loopback carrier sends chunks");
        };
        sent.push((frame.seq, frame.offset, frame.last, frame.data.clone()));

        let abs_path = if frame.last { WORKER_PATH } else { "" };
        let settled = upload
            .settle(&AttachmentTransferAck {
                bytes_received: frame.offset + frame.data.len() as u64,
                abs_path: abs_path.to_owned(),
                chunk_sha256: frame.chunk_sha256.clone(),
            })
            .expect("an accepting worker settles every chunk");
        progress.push(settled.bytes_received);
        assert!(carrier.sent_chunk());
    }

    assert_eq!(
        sent.iter()
            .map(|(seq, offset, last, _)| (*seq, *offset, *last))
            .collect::<Vec<_>>(),
        vec![(0, 0, false), (1, DIRECT_CHUNK_BYTES, true)]
    );
    assert_eq!(progress, vec![DIRECT_CHUNK_BYTES, total]);
    assert_eq!(
        upload.outcome().map(|result| result.abs_path).as_deref(),
        Some(WORKER_PATH)
    );
    let crossed: Vec<u8> = sent
        .iter()
        .flat_map(|(_, _, _, data)| data.clone())
        .collect();
    assert_eq!(
        crossed, file,
        "the slices cross the socket in order and whole"
    );
}

#[test]
fn requests_direct_status_on_the_authenticated_control_socket() {
    let mut carrier = LoopbackTransfer::new("worker-a", grant(0));
    let _hello = carrier.socket_opened().expect("the socket authenticates");
    carrier
        .frame_received(&ready_frame_bytes())
        .expect("the worker admits the carrier");

    let request_bytes = carrier
        .request_status("upload-a")
        .expect("a ready carrier may ask for a receipt");
    let request = AttachmentTransferClientFrame::decode_from_slice(&request_bytes)
        .expect("the request is a client frame");
    let Some(ClientFrame::StatusRequest(request)) = request.frame else {
        panic!("the control socket carries the status request");
    };
    assert_eq!(request.upload_id, "upload-a");

    let status = server_frame_bytes(ServerFrame::Status(Box::new(ProtoStatus {
        upload_id: "upload-a".to_owned(),
        next_seq: 0,
        bytes_received: 0,
        last_chunk_sha256: String::new(),
        committed: false,
        abs_path: String::new(),
        error: String::new(),
    })));
    let outcome = carrier.frame_received(&status);
    let Ok(ConversationOutcome::Status(receipt)) = outcome else {
        panic!("the worker's status settles the receipt in flight");
    };
    let AttachmentTransferStatus {
        __buffa_unknown_fields: Default::default(),
        upload_id,
        next_seq,
        bytes_received,
        last_chunk_sha256,
        committed,
        abs_path,
        error,
    } = *receipt;
    assert_eq!(upload_id, "upload-a");
    assert_eq!(next_seq, 0);
    assert_eq!(bytes_received, 0);
    assert!(last_chunk_sha256.is_empty());
    assert!(!committed);
    assert!(abs_path.is_empty());
    assert!(error.is_empty());
}
