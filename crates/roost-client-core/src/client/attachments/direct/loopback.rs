//! One authenticated loopback transport for one direct attachment upload: the
//! peer conversation with no packetisation, because a loopback socket carries a
//! whole frame. Ported from `attachment-loopback.ts`. Depends on
//! `conversation` for the frame rules; the socket, the read and the write are
//! the host's.

use roost_proto::buffa::Message;
use roost_proto::{
    AttachmentTransferChunk, AttachmentTransferClientFrame, AttachmentTransferServerFrame,
    AttachmentTransferStatusRequest,
};
use roost_proto::__buffa::oneof::attachment_transfer_client_frame::Frame as ClientFrame;
use roost_proto::__buffa::oneof::attachment_transfer_server_frame::Frame as ServerFrame;

use crate::client::attachments::conversation::{AttachmentConversation, ConversationOutcome};
use crate::client::attachments::grant::AttachmentDirectGrant;
use crate::client::attachments::transfer::receipt::AttachmentTransferStatus;
use crate::client::attachments::transfer::{AttachmentTransferCarrierError, InFlightChunk};

/// The path on the worker's door that speaks the attachment protocol.
pub const LOOPBACK_PATH: &str = "/ws/local-attachment-transfer";

/// The subprotocol the loopback socket negotiates. It names the attachment
/// protocol rather than the terminal one, so a door serving both cannot be
/// mistaken for the other.
pub const LOOPBACK_SUBPROTOCOL: &str = "roost-local-attachment-transfer-v1";

/// How long the worker has to answer the hello before the socket is retired.
pub const SETUP_DEADLINE_MS: u64 = 3_000;

/// How long one chunk's acknowledgement may take once the socket write is out.
pub const ACK_DEADLINE_MS: u64 = 15_000;

/// How long one status request may take.
pub const STATUS_DEADLINE_MS: u64 = 8_000;

/// The loopback deadlines a host arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopbackDeadline {
    Setup,
    Ack,
    Status,
}

/// The loopback carrier for one grant against one door.
#[derive(Debug)]
pub struct LoopbackTransfer {
    conversation: AttachmentConversation,
    grant: AttachmentDirectGrant,
    closed: bool,
}

impl LoopbackTransfer {
    /// A carrier bound to one door and one grant, before the socket exists.
    #[must_use]
    pub fn new(door_worker_fingerprint: &str, grant: AttachmentDirectGrant) -> Self {
        let conversation = AttachmentConversation::new(
            &grant,
            door_worker_fingerprint,
            "attachment loopback",
        );
        Self {
            conversation,
            grant,
            closed: false,
        }
    }

    /// The subprotocol the socket must be opened with.
    #[must_use]
    pub fn subprotocol(&self) -> &'static str {
        LOOPBACK_SUBPROTOCOL
    }

    /// Whether the worker's Ready has been admitted.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.conversation.is_ready()
    }

    /// Whether any chunk has been handed to the carrier.
    #[must_use]
    pub fn sent_chunk(&self) -> bool {
        self.conversation.sent_chunk()
    }

    /// The socket opened: authenticate, and arm the setup deadline for it.
    ///
    /// The loopback hello carries an EMPTY peer id, which is how the worker
    /// tells the two direct routes apart. Everything else in the frame is the
    /// grant's own tuple, so a worker can reject this upload without a lookup.
    pub fn socket_opened(&mut self) -> Result<Vec<u8>, AttachmentTransferCarrierError> {
        if self.closed {
            return Err(AttachmentTransferCarrierError::refused(
                "attachment loopback could not authenticate",
                self.sent_chunk(),
            ));
        }
        Ok(encode_client_frame(ClientFrame::Hello(Box::new(self.grant.hello("")))))
    }

    /// Take the carrier for one chunk and frame it.
    pub fn send_chunk(
        &mut self,
        chunk: &InFlightChunk,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, AttachmentTransferCarrierError> {
        self.conversation.begin_chunk(chunk.seq)?;
        let frame = AttachmentTransferChunk {
            upload_id: chunk.upload_id.clone(),
            seq: chunk.seq,
            data,
            last: chunk.last,
            offset: chunk.offset,
            chunk_sha256: chunk.chunk_sha256.clone(),
            ..Default::default()
        };
        Ok(encode_client_frame(ClientFrame::Chunk(Box::new(frame))))
    }

    /// The socket write failed outright, so the bytes provably never left.
    ///
    /// This is the one place the carrier un-sends: `sent_chunk` is the fallback
    /// boundary, and a write that threw before it wrote has not crossed it.
    pub fn send_failed(&mut self, reason: &str) -> AttachmentTransferCarrierError {
        self.conversation.rollback_chunk();
        self.conversation.fail_ack(reason, false)
    }

    /// Take the carrier for one receipt and frame it.
    pub fn request_status(
        &mut self,
        upload_id: &str,
    ) -> Result<Vec<u8>, AttachmentTransferCarrierError> {
        self.conversation.begin_status(upload_id)?;
        let frame = AttachmentTransferStatusRequest {
            upload_id: upload_id.to_owned(),
            ..Default::default()
        };
        Ok(encode_client_frame(ClientFrame::StatusRequest(Box::new(frame))))
    }

    /// Accept one inbound frame.
    pub fn frame_received(
        &mut self,
        frame: &[u8],
    ) -> Result<ConversationOutcome, AttachmentTransferCarrierError> {
        let frame = AttachmentTransferServerFrame::decode_from_slice(frame)
            .map_err(|_| self.close("attachment loopback received an invalid frame"))?;
        if !self.conversation.is_ready() {
            let Some(inner) = frame.frame.as_ref() else {
                return Err(self.close("attachment loopback received an invalid frame"));
            };
            let ServerFrame::Ready(ready) = inner else {
                return Err(self.close("attachment loopback required Ready first"));
            };
            self.conversation.admit_ready(ready)?;
            return Ok(ConversationOutcome::Ready);
        }
        let Some(inner) = frame.frame.as_ref() else {
            return Err(self.close("attachment loopback received an invalid frame"));
        };
        match inner {
            ServerFrame::Ack(ack) => self.conversation.settle_ack(ack),
            ServerFrame::Status(status) => self.conversation.settle_status(&status_from(status)),
            ServerFrame::Closed(_) => Err(self.close("attachment loopback closed")),
            ServerFrame::Ready(_) => {
                Err(self.close("attachment loopback received an invalid frame"))
            }
        }
    }

    /// A deadline a host armed elapsed.
    pub fn deadline_elapsed(
        &mut self,
        deadline: LoopbackDeadline,
    ) -> Result<(), AttachmentTransferCarrierError> {
        match deadline {
            LoopbackDeadline::Setup => Err(self.close("attachment loopback setup timed out")),
            LoopbackDeadline::Ack => {
                // The bytes may already be on the worker's disk, so this is the
                // one acknowledgement failure a receipt can still answer for.
                Err(self
                    .conversation
                    .fail_ack("attachment loopback acknowledgement timed out", true))
            }
            LoopbackDeadline::Status => Err(self
                .conversation
                .fail_status("attachment loopback status timed out")),
        }
    }

    /// End the carrier and every waiter on it.
    pub fn close(&mut self, reason: &str) -> AttachmentTransferCarrierError {
        if self.closed {
            return self.conversation.close(reason);
        }
        self.closed = true;
        self.conversation.close(reason)
    }
}

/// The durable receipt, as the shared conversation names it.
fn status_from(status: &roost_proto::AttachmentTransferStatus) -> AttachmentTransferStatus {
    AttachmentTransferStatus {
        upload_id: status.upload_id.clone(),
        next_seq: status.next_seq,
        bytes_received: status.bytes_received,
        last_chunk_sha256: status.last_chunk_sha256.clone(),
        committed: status.committed,
        abs_path: status.abs_path.clone(),
        error: status.error.clone(),
    }
}

/// Wrap one client-frame arm for encoding.
fn encode_client_frame(frame: ClientFrame) -> Vec<u8> {
    AttachmentTransferClientFrame {
        frame: Some(frame),
        ..Default::default()
    }
    .encode_to_vec()
}
