//! The authenticated frame conversation every direct attachment carrier shares.
//! Called by the loopback and peer carriers; Ready must come first, an ack must
//! name the chunk in flight, one bad frame ends the generation. Ported from the
//! frame-admission half of `attachment-loopback.ts` and `attachmentPeer.ts`.
//! Depends on `grant` for the tuple and `transfer` for the failure it reports.

use roost_proto::{AttachmentTransferAck as ProtoAttachmentTransferAck, AttachmentTransferReady};

use super::grant::AttachmentDirectGrant;
use super::transfer::receipt::AttachmentTransferStatus;
use super::transfer::{AttachmentTransferAck, AttachmentTransferCarrierError};

/// What the conversation did with one server frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationOutcome {
    /// Ready matched the authenticated tuple; the carrier may now send.
    Ready,
    /// An acknowledgement settled the chunk in flight.
    Ack(AttachmentTransferAck),
    /// A status settled the receipt in flight.
    Status(Box<AttachmentTransferStatus>),
    /// A frame that changes nothing, such as an acknowledgement arriving while
    /// a receipt is the outstanding question.
    Ignored,
}

/// The frame rules both direct carriers share, with no transport in it.
///
/// `carrier` is the two-word name the route is known by — `attachment
/// loopback`, `attachment peer` — and it is what the refusals are worded with,
/// so a log names the route the user was actually on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentConversation {
    carrier: &'static str,
    upload_id: String,
    worker_fingerprint: String,
    worker_epoch: String,
    session_id: String,
    ready: bool,
    closed: bool,
    sent_chunk: bool,
    ack_seq: Option<u32>,
    status_upload_id: Option<String>,
}

impl AttachmentConversation {
    /// Bind to the tuple the grant names. `worker_fingerprint` is the worker's,
    /// which on loopback is also the one the local door announced.
    #[must_use]
    pub fn new(
        grant: &AttachmentDirectGrant,
        worker_fingerprint: &str,
        carrier: &'static str,
    ) -> Self {
        Self {
            carrier,
            upload_id: grant.request.upload_id.clone(),
            worker_fingerprint: worker_fingerprint.to_owned(),
            worker_epoch: grant.worker_epoch.clone(),
            session_id: grant.request.session_id.clone(),
            ready: false,
            closed: false,
            sent_chunk: false,
            ack_seq: None,
            status_upload_id: None,
        }
    }

    /// Whether the worker's Ready has been admitted, which is what lets a chunk
    /// be sent at all.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Whether this generation has ended.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Whether any chunk has been handed to the carrier.
    #[must_use]
    pub fn sent_chunk(&self) -> bool {
        self.sent_chunk
    }

    /// The sequence whose acknowledgement is outstanding, if any.
    #[must_use]
    pub fn awaiting_ack(&self) -> Option<u32> {
        self.ack_seq
    }

    /// The upload whose receipt is outstanding, if any.
    #[must_use]
    pub fn awaiting_status(&self) -> Option<&str> {
        self.status_upload_id.as_deref()
    }

    /// Take the carrier for one chunk. One at a time, because the lanes are
    /// ordered and a second unacknowledged chunk makes a partially-received one
    /// impossible to detect.
    pub fn begin_chunk(&mut self, seq: u32) -> Result<(), AttachmentTransferCarrierError> {
        if self.closed || !self.ready || self.ack_seq.is_some() {
            let reason = format!("{} cannot send a chunk", self.carrier);
            return Err(AttachmentTransferCarrierError::refused(&reason, self.sent_chunk));
        }
        self.ack_seq = Some(seq);
        self.sent_chunk = true;
        Ok(())
    }

    /// Take the carrier for one receipt. Also one at a time.
    pub fn begin_status(&mut self, upload_id: &str) -> Result<(), AttachmentTransferCarrierError> {
        if self.closed || !self.ready || self.status_upload_id.is_some() {
            let reason = format!("{} cannot request status", self.carrier);
            return Err(AttachmentTransferCarrierError::refused(&reason, self.sent_chunk));
        }
        self.status_upload_id = Some(upload_id.to_owned());
        Ok(())
    }

    /// Admit a worker's Ready, or end the generation.
    pub fn admit_ready(
        &mut self,
        ready: &AttachmentTransferReady,
    ) -> Result<(), AttachmentTransferCarrierError> {
        if ready.worker_fingerprint != self.worker_fingerprint
            || ready.worker_epoch != self.worker_epoch
            || ready.session_id != self.session_id
            || ready.upload_id != self.upload_id
        {
            let reason = format!("{} Ready did not match its authenticated tuple", self.carrier);
            return Err(self.close(&reason));
        }
        self.ready = true;
        Ok(())
    }

    /// Settle the chunk in flight from the worker's acknowledgement.
    ///
    /// An acknowledgement naming a different chunk, or arriving when no chunk
    /// is in flight, ends the generation: the worker and this client no longer
    /// agree about which bytes are where, and a carrier in that state must not
    /// be asked to send again.
    pub fn settle_ack(
        &mut self,
        ack: &ProtoAttachmentTransferAck,
    ) -> Result<ConversationOutcome, AttachmentTransferCarrierError> {
        let Some(waited) = self.ack_seq else {
            return Ok(ConversationOutcome::Ignored);
        };
        if ack.upload_id != self.upload_id || ack.seq != waited {
            let reason = format!("{} acknowledged an unexpected chunk", self.carrier);
            return Err(self.close(&reason));
        }
        if !ack.error.is_empty() {
            let reason = format!("{} rejected a chunk", self.carrier);
            return Err(self.fail_ack(&reason, false));
        }
        self.ack_seq = None;
        Ok(ConversationOutcome::Ack(AttachmentTransferAck {
            bytes_received: ack.bytes_received,
            abs_path: ack.abs_path.clone(),
            chunk_sha256: ack.chunk_sha256.clone(),
        }))
    }

    /// Settle the receipt in flight from a durable status, or end the
    /// generation when the status names a different upload.
    pub fn settle_status(
        &mut self,
        status: &AttachmentTransferStatus,
    ) -> Result<ConversationOutcome, AttachmentTransferCarrierError> {
        if self.status_upload_id.as_deref() != Some(status.upload_id.as_str()) {
            let reason = format!("{} returned invalid status", self.carrier);
            return Err(self.close(&reason));
        }
        self.status_upload_id = None;
        Ok(ConversationOutcome::Status(Box::new(status.clone())))
    }

    /// End the generation, reporting the reason and whether bytes left.
    pub fn close(&mut self, reason: &str) -> AttachmentTransferCarrierError {
        if self.closed {
            return AttachmentTransferCarrierError::refused(reason, self.sent_chunk);
        }
        self.closed = true;
        self.ack_seq = None;
        self.status_upload_id = None;
        AttachmentTransferCarrierError::refused(reason, self.sent_chunk)
    }

    /// The write that carried this chunk failed before it wrote, so the bytes
    /// provably never left and the fallback boundary is still ahead of us.
    pub fn rollback_chunk(&mut self) {
        self.sent_chunk = false;
        self.ack_seq = None;
    }

    /// Fail the outstanding acknowledgement, with or without ambiguity.
    ///
    /// `ambiguous` is the distinction the whole receipt path turns on: a send
    /// whose acknowledgement never arrived may already be committed, and only a
    /// durable receipt may settle it.
    pub fn fail_ack(&mut self, reason: &str, ambiguous: bool) -> AttachmentTransferCarrierError {
        self.ack_seq = None;
        if ambiguous {
            AttachmentTransferCarrierError::ambiguous(reason)
        } else {
            AttachmentTransferCarrierError::refused(reason, self.sent_chunk)
        }
    }

    /// Fail the outstanding receipt, which is never ambiguous: a status
    /// request carries no upload bytes.
    pub fn fail_status(&mut self, reason: &str) -> AttachmentTransferCarrierError {
        self.status_upload_id = None;
        AttachmentTransferCarrierError::refused(reason, self.sent_chunk)
    }
}
