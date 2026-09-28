//! What one attachment operation answers a carrier with: the receipt for an
//! accepted chunk, the status a lost acknowledgement is recovered from, and
//! the refusal vocabulary both carry. Ports v2
//! `apps/worker/src/attachments/attachment-operation-receipts.ts` and the
//! refusal messages of `attachment-upload.ts`. Pure: no file, disk or clock.
//! Read by the operation owner, the upload facade and the direct sockets.

use roost_proto::AttachmentTransferStatus;
use roost_protocol::attachment_transfer::TransferErrorReason;

use super::journal::AttachmentOperationJournal;

/// Why an operation refused a chunk. v2 `AttachmentOperationError`, the
/// operation-owned subset of the transfer refusal reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentOperationError {
    UploadNotFound,
    UploadMismatch,
    ChunkOutOfOrder,
    ChunkOffsetMismatch,
    ChunkSha256Mismatch,
    TotalBytesMismatch,
    WriteFailed,
}

impl AttachmentOperationError {
    const ALL: [Self; 7] = [
        Self::UploadNotFound,
        Self::UploadMismatch,
        Self::ChunkOutOfOrder,
        Self::ChunkOffsetMismatch,
        Self::ChunkSha256Mismatch,
        Self::TotalBytesMismatch,
        Self::WriteFailed,
    ];

    /// The code a journal, a status and a direct acknowledgement spell.
    pub fn as_str(self) -> &'static str {
        self.reason().as_str()
    }

    /// v2 `ERROR_MESSAGE`: what a coordinator relay's `rpc-error` says.
    pub fn message(self) -> &'static str {
        match self {
            Self::UploadNotFound => "upload is unavailable",
            Self::UploadMismatch => "upload metadata does not match",
            Self::ChunkOutOfOrder => "attachment chunk is out of order",
            Self::ChunkOffsetMismatch => "attachment chunk offset does not match",
            Self::ChunkSha256Mismatch => "attachment chunk digest does not match",
            Self::TotalBytesMismatch => "attachment size does not match declared total",
            Self::WriteFailed => "attachment write failed",
        }
    }

    /// The transfer refusal a direct carrier closes with.
    pub fn reason(self) -> TransferErrorReason {
        match self {
            Self::UploadNotFound => TransferErrorReason::UploadNotFound,
            Self::UploadMismatch => TransferErrorReason::UploadMismatch,
            Self::ChunkOutOfOrder => TransferErrorReason::ChunkOutOfOrder,
            Self::ChunkOffsetMismatch => TransferErrorReason::ChunkOffsetMismatch,
            Self::ChunkSha256Mismatch => TransferErrorReason::ChunkSha256Mismatch,
            Self::TotalBytesMismatch => TransferErrorReason::TotalBytesMismatch,
            Self::WriteFailed => TransferErrorReason::WriteFailed,
        }
    }

    /// v2 `isOperationError`.
    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|error| error.as_str() == value)
    }
}

/// What one accepted chunk moved the operation to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentOperationReceipt {
    pub seq: u32,
    pub next_seq: u32,
    pub bytes_received: u64,
    pub chunk_sha256: String,
    pub committed: bool,
    pub abs_path: String,
}

/// An operation's durable progress, as a lost acknowledgement is recovered
/// from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentOperationStatus {
    pub upload_id: String,
    pub next_seq: u32,
    pub bytes_received: u64,
    pub last_chunk_sha256: String,
    pub committed: bool,
    pub abs_path: String,
    pub error: Option<AttachmentOperationError>,
}

impl AttachmentOperationStatus {
    /// The wire's empty-string-for-none spelling.
    pub fn error_str(&self) -> &'static str {
        self.error.map_or("", AttachmentOperationError::as_str)
    }

    pub fn to_proto(&self) -> AttachmentTransferStatus {
        AttachmentTransferStatus {
            upload_id: self.upload_id.clone(),
            next_seq: self.next_seq,
            bytes_received: self.bytes_received,
            last_chunk_sha256: self.last_chunk_sha256.clone(),
            committed: self.committed,
            abs_path: self.abs_path.clone(),
            error: self.error_str().to_owned(),
            ..Default::default()
        }
    }
}

/// What the operation owner settles a chunk with.
pub type AttachmentOperationResult = Result<AttachmentOperationReceipt, AttachmentOperationError>;

pub fn receipt_from_journal(journal: &AttachmentOperationJournal) -> AttachmentOperationReceipt {
    AttachmentOperationReceipt {
        seq: wire_seq(journal.next_seq.saturating_sub(1)),
        next_seq: wire_seq(journal.next_seq),
        bytes_received: journal.bytes_written,
        chunk_sha256: journal.last_chunk_sha256.clone(),
        committed: journal.committed,
        abs_path: journal.abs_path.clone(),
    }
}

pub fn status_from_journal(journal: &AttachmentOperationJournal) -> AttachmentOperationStatus {
    AttachmentOperationStatus {
        upload_id: journal.request_id.clone(),
        next_seq: wire_seq(journal.next_seq),
        bytes_received: journal.bytes_written,
        last_chunk_sha256: journal.last_chunk_sha256.clone(),
        committed: journal.committed,
        abs_path: journal.abs_path.clone(),
        error: (!journal.error.is_empty()).then(|| journal_error(journal)),
    }
}

pub fn empty_status(upload_id: &str, error: AttachmentOperationError) -> AttachmentOperationStatus {
    AttachmentOperationStatus {
        upload_id: upload_id.to_owned(),
        next_seq: 0,
        bytes_received: 0,
        last_chunk_sha256: String::new(),
        committed: false,
        abs_path: String::new(),
        error: Some(error),
    }
}

/// A journal written by another build may carry an error this one does not
/// know; it reads as `write_failed`.
pub fn journal_error(journal: &AttachmentOperationJournal) -> AttachmentOperationError {
    AttachmentOperationError::parse(&journal.error).unwrap_or(AttachmentOperationError::WriteFailed)
}

/// A journal counter as the wire's `uint32` carries it. Only a hand-edited
/// journal exceeds it; saturating keeps such a status from wrapping to a small
/// number that reads as real progress.
fn wire_seq(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
