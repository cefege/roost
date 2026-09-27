//! The loopback carrier, end to end over a fake transport.
//!
//! The loopback route is a socket to the local worker, so it is unframed: a
//! whole chunk crosses as one client frame, and the carrier tells the two routes
//! apart by authenticating with an empty peer id. The framed peer route is in
//! `attachment_carriers_peer.rs`; the worker end of this conversation is
//! `attachment_carriers_support`.
//!
//! The v2 names are kept verbatim. The mutation experiment, in the slice report:
//! drop the `worker_epoch` comparison from `AttachmentConversation::admit_ready`
//! and a Ready naming another epoch slips through.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachment_carriers_support;

use attachment_carriers_support::{
    DIGEST, WORKER_PATH, grant, ready_frame_bytes, server_frame_bytes,
};
use roost_client_core::client::attachments::conversation::ConversationOutcome;
use roost_client_core::client::attachments::direct::LocalWorkerDoor;
use roost_client_core::client::attachments::direct::loopback::{
    LOOPBACK_PATH, LOOPBACK_SUBPROTOCOL, LoopbackTransfer,
};
use roost_client_core::client::attachments::transfer::receipt::AttachmentTransferStatus;
use roost_client_core::client::attachments::transfer::{
    AttachmentTransferAck, DIRECT_CHUNK_BYTES, DirectUpload,
};
use roost_proto::__buffa::oneof::attachment_transfer_client_frame::Frame as ClientFrame;
use roost_proto::__buffa::oneof::attachment_transfer_server_frame::Frame as ServerFrame;
use roost_proto::buffa::Message;
use roost_proto::{AttachmentTransferClientFrame, AttachmentTransferStatus as ProtoStatus};

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
            .begin_chunk(data.clone(), DIGEST)
            .expect("the slice is the one that was asked for");
        let in_flight = upload.in_flight().expect("a chunk is in flight").clone();
        let frame_bytes = carrier
            .send_chunk(&in_flight, data)
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
        __buffa_unknown_fields: Default::default(),
    })));
    let outcome = carrier.frame_received(&status);
    let Ok(ConversationOutcome::Status(receipt)) = outcome else {
        panic!("the worker's status settles the receipt in flight");
    };
    let AttachmentTransferStatus {
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
