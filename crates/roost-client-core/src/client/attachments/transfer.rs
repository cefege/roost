//! The chunking, the digest and the ACK rule every direct carrier shares. A host
//! reads the slice, hashes it and sends the frame this module hands it; this
//! module decides which slice comes next and whether an ack settles it. Ported
//! from `attachmentTransfer.ts`. The card transitions are in `transfer::ledger`
//! and the lost-ack receipts in `transfer::receipt`.

use std::fmt;

pub mod ledger;
pub mod receipt;

/// Bytes per direct chunk, and the number the grant's `total_bytes` is checked
/// against.
pub const DIRECT_CHUNK_BYTES: u64 = 512 * 1024;

/// The length of the lowercase hex SHA-256 echoed on every chunk.
pub const CHUNK_SHA256_HEX_LENGTH: usize = 64;

/// The close reason for a settled upload, and the only one that means the
/// worker committed the file.
pub const COMPLETE_REASON: &str = "complete";

/// The close reason for anything else. It is a resource release either way;
/// only the string tells a log which of the two happened.
pub const FAILED_REASON: &str = "attachment transfer failed";

/// The reason used when a settlement is asked for with nothing in flight. It is
/// a caller bug rather than a carrier failure, and it is not ambiguous: no
/// bytes were sent.
const NOTHING_IN_FLIGHT_REASON: &str = "attachment transfer has no chunk in flight";

/// The largest total a file may declare and still be uploadable directly.
///
/// v2 refused a `File.size` that was not a JavaScript safe integer before it
/// minted a grant, because that size crosses the wire as a number a peer reads
/// back. A host that hands this module a total above it is handing it a size no
/// peer could read exactly, and the upload would be refused at the far end
/// instead of here.
pub const MAX_SAFE_TOTAL_BYTES: u64 = 9_007_199_254_740_991;

/// Why a carrier failed, and — the load-bearing part — whether upload bytes
/// left the browser before it did.
///
/// `sent_chunk` is what decides the direct-then-peer fallback: a carrier that
/// failed before its first chunk is a route that was never available, and one
/// that failed after is a route whose state only its own status control can
/// settle. `ambiguous` is the narrower case: the send was attempted and its
/// acknowledgement never arrived, so the bytes may or may not be committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentTransferCarrierError {
    pub reason: String,
    pub sent_chunk: bool,
    pub ambiguous: bool,
}

impl AttachmentTransferCarrierError {
    /// A carrier that could not be used, before or after bytes left.
    #[must_use]
    pub fn refused(reason: &str, sent_chunk: bool) -> Self {
        Self {
            reason: reason.to_owned(),
            sent_chunk,
            ambiguous: false,
        }
    }

    /// A send whose acknowledgement never arrived: the bytes may already be
    /// committed, so the caller must ask a receipt rather than re-send.
    #[must_use]
    pub fn ambiguous(reason: &str) -> Self {
        Self {
            reason: reason.to_owned(),
            sent_chunk: true,
            ambiguous: true,
        }
    }

    /// The send stays unconfirmed: neither receipt source answered.
    #[must_use]
    pub fn unconfirmed() -> Self {
        Self::ambiguous("attachment transfer acknowledgement was not confirmed")
    }
}

impl fmt::Display for AttachmentTransferCarrierError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.reason)
    }
}

impl std::error::Error for AttachmentTransferCarrierError {}

/// Whether a string is a chunk digest this pipeline will send or accept.
///
/// Exactly the lowercase hex of one SHA-256. An uppercase digest is refused
/// rather than folded: the digest is compared byte-for-byte against what the
/// worker computed, so two spellings of one digest would look like corruption.
#[must_use]
pub fn is_chunk_sha256(value: &str) -> bool {
    value.len() == CHUNK_SHA256_HEX_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// One chunk, as the host frames and sends it. Building it moves nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentTransferChunk {
    pub upload_id: String,
    pub seq: u32,
    /// The byte offset this chunk starts at.
    pub offset: u64,
    pub data: Vec<u8>,
    pub chunk_sha256: String,
    /// Whether this is the slice that reaches the end of the file.
    pub last: bool,
}

/// The worker's receipt for one chunk, in the form the transfer settles from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentTransferAck {
    pub bytes_received: u64,
    /// Populated only on the final chunk's acknowledgement.
    pub abs_path: String,
    pub chunk_sha256: String,
}

/// A settled upload: the path the worker committed the file to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentTransferResult {
    pub abs_path: String,
}

/// The slice a host must read and hash next.
///
/// A request rather than bytes: this module never holds the file, so an upload
/// of any size costs one slice on the host and nothing here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SliceRequest {
    pub seq: u32,
    pub offset: u64,
    /// How many bytes the host must read at `offset`. Zero only for a
    /// zero-byte file, whose single final chunk is empty.
    pub bytes: usize,
    pub last: bool,
}

/// The chunk this transfer is waiting on, and everything a receipt must match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlightChunk {
    pub upload_id: String,
    pub seq: u32,
    pub offset: u64,
    /// The chunk's length, which is also how far the offset advances when it
    /// settles.
    pub bytes: usize,
    pub chunk_sha256: String,
    pub last: bool,
}

impl InFlightChunk {
    /// The bytes the worker must have written once this chunk is durable.
    #[must_use]
    pub fn expected_bytes(&self) -> u64 {
        self.offset + self.bytes as u64
    }

    /// The frame to send, given the bytes the host read and hashed.
    #[must_use]
    pub fn frame(&self, data: Vec<u8>) -> AttachmentTransferChunk {
        AttachmentTransferChunk {
            upload_id: self.upload_id.clone(),
            seq: self.seq,
            offset: self.offset,
            data,
            chunk_sha256: self.chunk_sha256.clone(),
            last: self.last,
        }
    }
}

/// Why a chunk could not be framed from what the host handed back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkFramingRefusal {
    /// The upload already finished; there is no next slice.
    UploadComplete,
    /// A chunk is already in flight, so this would be a second unacknowledged
    /// send on an ordered route.
    AlreadyInFlight,
    /// The host returned a different number of bytes than it was asked for.
    SliceLengthMismatch { expected: usize, actual: usize },
    /// The host returned something that is not one SHA-256 in lowercase hex.
    DigestMalformed,
}

impl fmt::Display for ChunkFramingRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UploadComplete => formatter.write_str("the upload already finished"),
            Self::AlreadyInFlight => formatter.write_str("a chunk is already in flight"),
            Self::SliceLengthMismatch { expected, actual } => {
                write!(formatter, "expected {expected} bytes, got {actual}")
            }
            Self::DigestMalformed => formatter.write_str("the chunk digest is malformed"),
        }
    }
}

impl std::error::Error for ChunkFramingRefusal {}

/// What settling a chunk decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSettled {
    /// What the worker says it has, which is what the card reports.
    pub bytes_received: u64,
    /// Whether this was the final chunk.
    pub completed: bool,
    /// The committed path, populated only when `completed`.
    pub abs_path: String,
}

/// One serial direct upload on one carrier.
///
/// The state is where the next slice starts, which sequence it is, and what is
/// waiting to be acknowledged. The file itself is never held here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectUpload {
    upload_id: String,
    total_bytes: u64,
    offset: u64,
    seq: u32,
    in_flight: Option<InFlightChunk>,
    abs_path: String,
    completed: bool,
}

impl DirectUpload {
    /// Begin an upload of `total_bytes` under `upload_id`.
    #[must_use]
    pub fn new(upload_id: &str, total_bytes: u64) -> Self {
        Self {
            upload_id: upload_id.to_owned(),
            total_bytes,
            offset: 0,
            seq: 0,
            in_flight: None,
            abs_path: String::new(),
            completed: false,
        }
    }

    /// Whether any chunk has been handed to a carrier.
    ///
    /// The fallback boundary. A carrier that fails while this is false was
    /// never available; one that fails after it is true may hold bytes the
    /// worker has already committed.
    #[must_use]
    pub fn sent_chunk(&self) -> bool {
        self.seq > 0
    }

    /// The slice to read and hash next, or `None` once the final chunk has
    /// settled.
    ///
    /// A zero-byte file answers with one empty final slice rather than none,
    /// because a file with no chunks is a file that was never created.
    #[must_use]
    pub fn next_slice(&self) -> Option<SliceRequest> {
        if self.completed || self.in_flight.is_some() {
            return None;
        }
        // The loop's own condition: the first slice always exists, and every
        // later one exists only while the offset is inside the file.
        if self.offset > 0 && self.offset >= self.total_bytes {
            return None;
        }
        let remaining = self.total_bytes - self.offset;
        Some(SliceRequest {
            seq: self.seq,
            offset: self.offset,
            bytes: remaining.min(DIRECT_CHUNK_BYTES) as usize,
            last: self.offset + DIRECT_CHUNK_BYTES >= self.total_bytes,
        })
    }

    /// Frame the slice the host just read, and hold it until it settles.
    ///
    /// The two refusals that are not the host's to fix: a slice of the wrong
    /// length shifts every later offset, and a bad digest is only caught by the
    /// worker after the bytes crossed.
    pub fn begin_chunk(
        &mut self,
        data: Vec<u8>,
        chunk_sha256: &str,
    ) -> Result<(), ChunkFramingRefusal> {
        let request = self.next_slice().ok_or({
            if self.completed {
                ChunkFramingRefusal::UploadComplete
            } else {
                ChunkFramingRefusal::AlreadyInFlight
            }
        })?;
        if data.len() != request.bytes {
            return Err(ChunkFramingRefusal::SliceLengthMismatch {
                expected: request.bytes,
                actual: data.len(),
            });
        }
        if !is_chunk_sha256(chunk_sha256) {
            return Err(ChunkFramingRefusal::DigestMalformed);
        }
        self.in_flight = Some(InFlightChunk {
            upload_id: self.upload_id.clone(),
            seq: request.seq,
            offset: request.offset,
            bytes: request.bytes,
            chunk_sha256: chunk_sha256.to_owned(),
            last: request.last,
        });
        Ok(())
    }

    /// The chunk waiting to be acknowledged.
    #[must_use]
    pub fn in_flight(&self) -> Option<&InFlightChunk> {
        self.in_flight.as_ref()
    }

    /// Settle the in-flight chunk from the worker's acknowledgement, or refuse
    /// it.
    ///
    /// A path on a non-final acknowledgement, or a final one without it, means
    /// the client and the worker disagree about where the file ends.
    pub fn settle(
        &mut self,
        ack: &AttachmentTransferAck,
    ) -> Result<ChunkSettled, AttachmentTransferCarrierError> {
        let chunk = self
            .in_flight
            .clone()
            .ok_or_else(|| {
                AttachmentTransferCarrierError::refused(NOTHING_IN_FLIGHT_REASON, self.sent_chunk())
            })?;
        if ack.bytes_received != chunk.expected_bytes()
            || !is_chunk_sha256(&ack.chunk_sha256)
            || ack.chunk_sha256 != chunk.chunk_sha256
            || !ack.abs_path.is_empty() != chunk.last
        {
            return Err(AttachmentTransferCarrierError::refused(
                "attachment transfer acknowledged invalid chunk state",
                true,
            ));
        }
        self.offset = chunk.expected_bytes();
        self.seq += 1;
        self.in_flight = None;
        if chunk.last {
            self.completed = true;
            self.abs_path = ack.abs_path.clone();
        }
        Ok(ChunkSettled {
            bytes_received: ack.bytes_received,
            completed: chunk.last,
            abs_path: self.abs_path.clone(),
        })
    }

    /// The settled outcome, or `None` while the upload is still running.
    #[must_use]
    pub fn outcome(&self) -> Option<AttachmentTransferResult> {
        self.completed.then(|| AttachmentTransferResult {
            abs_path: self.abs_path.clone(),
        })
    }

    /// Why this carrier is being closed.
    #[must_use]
    pub fn close_reason(&self) -> &'static str {
        if self.completed {
            COMPLETE_REASON
        } else {
            FAILED_REASON
        }
    }
}
