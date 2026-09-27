//! The two receipts a lost acknowledgement may be settled from, in the order
//! they are asked, and the `next_seq` rule that decides whether either counts.
//! Ported from `recoverAcknowledgement` in `attachmentTransfer.ts`. A receipt
//! that settles a chunk is why an acknowledged chunk is never re-sent.

use super::{AttachmentTransferAck, InFlightChunk, is_chunk_sha256};

/// The durable receipt, which settles an ambiguous send without re-sending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentTransferStatus {
    pub upload_id: String,
    /// The next sequence the worker expects. A receipt that names a different
    /// one is about a chunk this client did not send.
    pub next_seq: u32,
    pub bytes_received: u64,
    pub last_chunk_sha256: String,
    pub committed: bool,
    pub abs_path: String,
    pub error: String,
}

/// Which route a receipt came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptSource {
    /// The carrier's own direct status control.
    Carrier,
    /// The authenticated coordinator's relay.
    Coordinator,
}

/// The receipts, in the order they are asked. A host walks this list and stops
/// at the first [`ReceiptOutcome::Settled`].
pub const RECEIPT_SOURCES: [ReceiptSource; 2] =
    [ReceiptSource::Carrier, ReceiptSource::Coordinator];

/// How long the authenticated coordinator may take to answer a receipt. The
/// relay is the slower of the two sources and the only one left once the
/// carrier is gone, so it gets the same bound the direct control does.
pub const COORDINATOR_STATUS_DEADLINE_MS: u64 = 8_000;

/// What one receipt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptOutcome {
    /// The receipt describes the in-flight chunk, so nothing is re-sent.
    Settled(AttachmentTransferAck),
    /// This source could not answer, so the next one is asked.
    TryNextSource,
    /// The last source could not answer and the send stays unconfirmed.
    Unconfirmed,
}

/// Settle an in-flight chunk from a durable receipt.
///
/// `status` is `None` when the probe itself failed, which is the common case
/// for the carrier source once its socket is gone. Every field is compared, and
/// `committed` must equal `last`: a receipt that committed a non-final chunk,
/// or left a final chunk uncommitted, describes a state the client never asked
/// for.
#[must_use]
pub fn settle_from_receipt(
    chunk: &InFlightChunk,
    source: ReceiptSource,
    status: Option<&AttachmentTransferStatus>,
) -> ReceiptOutcome {
    let Some(status) = status.filter(|status| {
        status.error.is_empty()
            && status.upload_id == chunk.upload_id
            && u64::from(status.next_seq) == u64::from(chunk.seq) + 1
            && status.bytes_received == chunk.expected_bytes()
            && is_chunk_sha256(&status.last_chunk_sha256)
            && status.last_chunk_sha256 == chunk.chunk_sha256
            && status.committed == chunk.last
            && !status.abs_path.is_empty() == chunk.last
    }) else {
        return match source {
            ReceiptSource::Carrier => ReceiptOutcome::TryNextSource,
            ReceiptSource::Coordinator => ReceiptOutcome::Unconfirmed,
        };
    };
    ReceiptOutcome::Settled(AttachmentTransferAck {
        bytes_received: status.bytes_received,
        abs_path: status.abs_path.clone(),
        chunk_sha256: status.last_chunk_sha256.clone(),
    })
}
