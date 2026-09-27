//! The fixtures both direct-carrier test binaries share: the peer test and the
//! loopback test drive one conversation over two different fake transports, and
//! the worker end of that conversation is the same worker, so the grant, the
//! chunk framing, and the frames it answers with are defined once here.
//!
//! The peer test additionally needs the packet layer — the header an idle
//! channel carries, and the assembler that puts it back together — because the
//! peer route is framed and the loopback route is not.

#![allow(dead_code)]

use roost_client_core::client::attachments::grant::{
    AttachmentDirectGrant, AttachmentDirectGrantRequest, AttachmentDirectGrantResponse,
};
use roost_client_core::client::attachments::packets::assembler::AttachmentPacketAssembler;
use roost_client_core::client::attachments::packets::{
    AttachmentPacketHeader, PeerLane, encode_attachment_packet,
};
use roost_client_core::client::attachments::peer::AttachmentPeerTransfer;
use roost_client_core::client::attachments::transfer::{DirectUpload, InFlightChunk};
use roost_proto::__buffa::oneof::attachment_transfer_server_frame::Frame as ServerFrame;
use roost_proto::buffa::Message;
use roost_proto::{
    AttachmentTransferAck as ProtoAck, AttachmentTransferReady, AttachmentTransferServerFrame,
};

pub const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The committed path the fake worker reports.
pub const WORKER_PATH: &str = "/worker/peer.bin";

/// The grant these tests' carriers authenticate with.
pub fn grant(total_bytes: u64) -> AttachmentDirectGrant {
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
pub fn first_chunk(total_bytes: u64) -> (DirectUpload, InFlightChunk) {
    let mut upload = DirectUpload::new("upload-a", total_bytes);
    let request = upload.next_slice().expect("an upload has a first slice");
    upload
        .begin_chunk(vec![request.bytes as u8; request.bytes], DIGEST)
        .expect("the first slice is frameable");
    let in_flight = upload.in_flight().expect("a chunk is in flight").clone();
    (upload, in_flight)
}

/// The worker's Ready, as the bytes a socket delivers.
pub fn ready_frame_bytes() -> Vec<u8> {
    server_frame_bytes(ready_frame())
}

/// Wrap a server frame in the one packet an idle channel would carry.
pub fn server_packet(message_id: u32, frame: ServerFrame) -> Vec<u8> {
    let bytes = server_frame_bytes(frame);
    let header = AttachmentPacketHeader {
        message_id,
        total_bytes: bytes.len() as u32,
        offset_bytes: 0,
    };
    encode_attachment_packet(header, &bytes).expect("a frame this size is encodable")
}

/// A worker's Ready for the tuple both carriers authenticate.
pub fn ready_frame() -> ServerFrame {
    ServerFrame::Ready(Box::new(AttachmentTransferReady {
        __buffa_unknown_fields: Default::default(),
        worker_fingerprint: "worker-a".to_owned(),
        worker_epoch: "epoch-a".to_owned(),
        session_id: "session-a".to_owned(),
        upload_id: "upload-a".to_owned(),
    }))
}

/// Any server frame, as the bytes a socket delivers.
pub fn server_frame_bytes(frame: ServerFrame) -> Vec<u8> {
    AttachmentTransferServerFrame {
        __buffa_unknown_fields: Default::default(),
        frame: Some(frame),
        ..Default::default()
    }
    .encode_to_vec()
}

/// The worker's acknowledgement for one chunk, echoing its own digest.
pub fn ack_frame(chunk: &InFlightChunk, bytes_received: u64) -> ServerFrame {
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
pub fn decode_client_lane(lane: PeerLane, packets: &[Vec<u8>], now_ms: u64) -> Vec<Vec<u8>> {
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
pub fn flush_once(
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
