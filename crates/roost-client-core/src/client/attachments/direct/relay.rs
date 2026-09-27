//! The coordinator relay: the carrier that is always left, so it has no
//! admission and no digest. Ported from the fallback loop in
//! `apps/web/src/lib/attachments.ts`. Depends on the same `SliceRequest` the
//! direct route uses, so a caller drives both the same way.

use crate::client::attachments::direct::AttachmentDirectUploadRequest;
use crate::client::attachments::transfer::{AttachmentTransferResult, SliceRequest};

/// Bytes per relay chunk. Four mebibytes: well under any Connect message limit,
/// and four times fewer ordered round trips than the direct route's slice.
pub const RELAY_CHUNK_BYTES: u64 = 4 * 1024 * 1024;

/// One relay chunk, as the host frames it for the coordinator call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayChunk {
    pub upload_id: String,
    pub session_id: String,
    pub filename: String,
    pub short_path: bool,
    pub seq: u32,
    pub offset: u64,
    pub data: Vec<u8>,
    pub last: bool,
}

/// One serial relay upload, in the same shape as the direct one so a caller
/// drives both the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayUpload {
    upload_id: String,
    session_id: String,
    filename: String,
    short_path: bool,
    total_bytes: u64,
    offset: u64,
    seq: u32,
    abs_path: String,
    completed: bool,
}

impl RelayUpload {
    /// Begin the relay upload for one file. The upload id is the same one the
    /// direct attempt would have used, so a card names one upload either way.
    #[must_use]
    pub fn new(request: &AttachmentDirectUploadRequest) -> Self {
        Self {
            upload_id: request.upload_id.clone(),
            session_id: request.session_id.clone(),
            filename: request.file_name.clone(),
            short_path: request.short_path,
            total_bytes: request.file_bytes,
            offset: 0,
            seq: 0,
            abs_path: String::new(),
            completed: false,
        }
    }

    /// The slice to read next, or `None` once the final chunk settled.
    ///
    /// A zero-byte file answers with one empty final slice: a file with no
    /// chunks is a file that was never created, on this route as much as the
    /// direct one.
    #[must_use]
    pub fn next_slice(&self) -> Option<SliceRequest> {
        if self.completed {
            return None;
        }
        if self.offset > 0 && self.offset >= self.total_bytes {
            return None;
        }
        let remaining = self.total_bytes - self.offset;
        Some(SliceRequest {
            seq: self.seq,
            offset: self.offset,
            bytes: remaining.min(RELAY_CHUNK_BYTES) as usize,
            last: self.offset + RELAY_CHUNK_BYTES >= self.total_bytes,
        })
    }

    /// Frame the slice the host read, and hold it until the coordinator
    /// answers.
    ///
    /// `None` means the upload already finished, which is the caller's cue
    /// that it sent one chunk too many rather than that the worker refused it.
    pub fn frame(&mut self, data: Vec<u8>) -> Option<RelayChunk> {
        let request = self.next_slice()?;
        self.offset += request.bytes as u64;
        self.seq += 1;
        Some(RelayChunk {
            upload_id: self.upload_id.clone(),
            session_id: self.session_id.clone(),
            filename: self.filename.clone(),
            short_path: self.short_path,
            seq: request.seq,
            offset: request.offset,
            data,
            last: request.last,
        })
    }

    /// Settle the chunk the coordinator answered for, and report the progress
    /// this route is willing to vouch for.
    ///
    /// That figure is the slice boundary, not the coordinator's byte count: it
    /// is what this client knows is true, and a decrease it cannot explain is
    /// the card's business, not this loop's.
    pub fn settle(&mut self, abs_path: &str) -> u64 {
        if self.completed {
            return self.total_bytes;
        }
        let progress = (self.offset + RELAY_CHUNK_BYTES).min(self.total_bytes);
        if !abs_path.is_empty() {
            self.completed = true;
            self.abs_path = abs_path.to_owned();
        }
        progress
    }

    /// The settled outcome, or `None` while the upload is still running.
    #[must_use]
    pub fn outcome(&self) -> Option<AttachmentTransferResult> {
        self.completed.then(|| AttachmentTransferResult {
            abs_path: self.abs_path.clone(),
        })
    }
}
