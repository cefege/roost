//! The peer carrier, end to end over a fake transport.
//!
//! The peer route is the framed one: two negotiated channels, packets under the
//! cap, and a carrier that cannot send a byte until the worker's Ready names
//! its own epoch. The loopback route is in `attachment_carriers_loopback.rs`;
//! the worker end of this conversation is `attachment_carriers_support`.
//! The packet framing underneath the peer route is in `attachment_packets.rs`.
//!
//! The v2 names are kept verbatim. The mutation experiment, in the slice report:
//! drop the `worker_epoch` comparison from `AttachmentConversation::admit_ready`
//! and a Ready naming another epoch slips through.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachment_carriers_support;

use attachment_carriers_support::{
    DIGEST, WORKER_PATH, ack_frame, decode_client_lane, first_chunk, flush_once, grant,
    ready_frame, server_packet,
};
use roost_client_core::client::attachments::conversation::ConversationOutcome;
use roost_client_core::client::attachments::packets::{ATTACHMENT_PACKET_MAX_BYTES, PeerLane};
use roost_client_core::client::attachments::peer::AttachmentPeerTransfer;
use roost_client_core::client::attachments::transfer::DIRECT_CHUNK_BYTES;
use roost_proto::__buffa::oneof::attachment_transfer_client_frame::Frame as ClientFrame;
use roost_proto::AttachmentTransferClientFrame;
use roost_proto::buffa::Message;

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
