//! Server-frame encoding for direct attachment ports: each function builds one
//! protobuf frame and hands it to the port's control lane, reporting refusal so
//! the caller can retire the route. `direct_sockets` decides what to send and
//! when. Ports `apps/worker/src/attachments/attachment-direct-frames.ts`.

use roost_proto::__buffa::oneof::attachment_transfer_server_frame::Frame;
use roost_proto::buffa::Message;
use roost_proto::{
    AttachmentTransferAck, AttachmentTransferClosed, AttachmentTransferReady,
    AttachmentTransferServerFrame,
};
use roost_protocol::attachment_transfer::PeerChannelLane;

use super::receipts::AttachmentOperationStatus;
use super::transfer_port::{AttachmentTransferPort, SendResult};

/// The identity a ready frame proves to the browser.
#[derive(Debug, Clone, Copy)]
pub(super) struct AttachmentReadyFields<'fields> {
    pub worker_fingerprint: &'fields str,
    pub worker_epoch: &'fields str,
    pub session_id: &'fields str,
    pub upload_id: &'fields str,
}

/// An operation receipt supplies these; a failure ack supplies them directly.
#[derive(Debug, Clone, Copy)]
pub(super) struct AttachmentAckFields<'fields> {
    pub seq: u32,
    pub bytes_received: u64,
    pub chunk_sha256: &'fields str,
}

pub(super) fn send_attachment_ready(
    port: &dyn AttachmentTransferPort,
    ready: AttachmentReadyFields<'_>,
) -> bool {
    send_server_frame(
        port,
        Frame::from(AttachmentTransferReady {
            worker_fingerprint: ready.worker_fingerprint.to_owned(),
            worker_epoch: ready.worker_epoch.to_owned(),
            session_id: ready.session_id.to_owned(),
            upload_id: ready.upload_id.to_owned(),
            ..Default::default()
        }),
    )
}

pub(super) fn send_attachment_ack(
    port: &dyn AttachmentTransferPort,
    upload_id: &str,
    ack: AttachmentAckFields<'_>,
    abs_path: &str,
    error: &str,
) -> bool {
    send_server_frame(
        port,
        Frame::from(AttachmentTransferAck {
            upload_id: upload_id.to_owned(),
            seq: ack.seq,
            bytes_received: ack.bytes_received,
            abs_path: abs_path.to_owned(),
            error: error.to_owned(),
            chunk_sha256: ack.chunk_sha256.to_owned(),
            ..Default::default()
        }),
    )
}

pub(super) fn send_attachment_status(
    port: &dyn AttachmentTransferPort,
    status: &AttachmentOperationStatus,
) -> bool {
    send_server_frame(port, Frame::from(status.to_proto()))
}

pub(super) fn send_attachment_closed(port: &dyn AttachmentTransferPort, reason: &str) -> bool {
    send_server_frame(
        port,
        Frame::from(AttachmentTransferClosed {
            reason: reason.to_owned(),
            ..Default::default()
        }),
    )
}

fn send_server_frame(port: &dyn AttachmentTransferPort, frame: Frame) -> bool {
    let bytes = AttachmentTransferServerFrame {
        frame: Some(frame),
        ..Default::default()
    }
    .encode_to_vec();
    port.send(bytes, PeerChannelLane::Control) != SendResult::Refused
}
