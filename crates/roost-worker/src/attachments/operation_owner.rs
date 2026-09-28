//! The one owner of every attachment operation's bytes and outcome, for both
//! carriers: a direct chunk's journal is written before its receipt, a relay's
//! progress stays in memory until its final commit. Ports v2
//! `apps/worker/src/attachments/attachment-operation-owner.ts` (its commit is
//! `operation_commit`, its opening `operation_open`). Called by the upload facade.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures_util::future::{BoxFuture, Shared};
use roost_protocol::attachment_transfer::is_chunk_sha256;
use sha2::{Digest, Sha256};

use super::file_hash::sha256_hex;
use super::journal::{
    AttachmentOperationJournal, AttachmentOperationLoad, AttachmentOperationPaths,
    MAX_SAFE_INTEGER, load_attachment_operation, persist_attachment_operation,
};
use super::operation_commit::{fail_journal, fail_operation, recover_finalization};
use super::operation_open::{restore, temp_matches_journal};
use super::receipts::{
    AttachmentOperationError, AttachmentOperationResult, AttachmentOperationStatus, empty_status,
    journal_error, receipt_from_journal, status_from_journal,
};
use super::store_paths::AttachmentBase;
use super::{AttachmentClock, Carrier, OperationDescriptor};

/// Matches the coordinator's pending-RPC deadline: once it gives up on a
/// silent upload, so does the worker.
pub const ATTACHMENT_OPERATION_IDLE: Duration = Duration::from_secs(5 * 60);

/// v2 bounds a carrier id by JavaScript string length.
const MAX_CARRIER_ID_UNITS: usize = 128;

/// One chunk offered to an operation.
#[derive(Debug, Clone)]
pub struct AttachmentOperationChunk {
    pub descriptor: OperationDescriptor,
    pub carrier: Carrier,
    pub carrier_id: String,
    pub seq: u32,
    pub offset: u64,
    pub data: Vec<u8>,
    pub last: bool,
    /// The digest the carrier claims for `data`. A coordinator relay claims
    /// none, and its digest is computed here once.
    pub chunk_sha256: Option<String>,
}

pub(super) type SharedCommit = Shared<BoxFuture<'static, AttachmentOperationResult>>;

/// An operation this process is writing, or a terminal one read from disk to
/// answer a chunk.
pub(super) struct ActiveOperation {
    pub(super) key: String,
    pub(super) paths: AttachmentOperationPaths,
    pub(super) journal: AttachmentOperationJournal,
    pub(super) file: Option<File>,
    pub(super) hasher: Sha256,
    pub(super) last_activity: Instant,
    /// Set while the final chunk's commit awaits the disk; the operation stays
    /// in the table and every later chunk for it waits on this.
    pub(super) commit: Option<SharedCommit>,
    /// Whether it belongs in the active table. A journal that is already
    /// terminal answers one chunk from disk and is never registered.
    pub(super) registered: bool,
}

pub(super) type ActiveTable = HashMap<String, ActiveOperation>;

/// What `accept` settled in the call, or the commit the answer waits on.
pub enum Accepted {
    Settled(AttachmentOperationResult),
    Pending(BoxFuture<'static, AttachmentOperationResult>),
}

impl std::fmt::Debug for Accepted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Settled(result) => formatter.debug_tuple("Settled").field(result).finish(),
            Self::Pending(_) => formatter.write_str("Pending"),
        }
    }
}

impl Accepted {
    pub async fn outcome(self) -> AttachmentOperationResult {
        match self {
            Self::Settled(result) => result,
            Self::Pending(commit) => commit.await,
        }
    }
}

pub(super) enum Step {
    Settle(AttachmentOperationResult),
    Wait(SharedCommit),
    Committing(SharedCommit),
}

enum Advance {
    Answer(AttachmentOperationResult),
    Fail(AttachmentOperationError),
    AwaitCommit(SharedCommit),
    Commit,
}

/// The process owner of all attachment byte carriers.
pub struct AttachmentOperationOwner {
    pub(super) base: AttachmentBase,
    pub(super) clock: AttachmentClock,
    active: Mutex<ActiveTable>,
}

impl std::fmt::Debug for AttachmentOperationOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentOperationOwner")
            .field("base", &self.base)
            .field("active", &self.lock_active().len())
            .finish()
    }
}

impl AttachmentOperationOwner {
    pub fn new(base: AttachmentBase, clock: AttachmentClock) -> Arc<Self> {
        Arc::new(Self {
            base,
            clock,
            active: Mutex::new(HashMap::new()),
        })
    }

    /// Everything up to the final commit's first flush runs in THIS call, under
    /// one lock, so chunks are written in the order they are accepted.
    pub fn accept(self: &Arc<Self>, chunk: AttachmentOperationChunk) -> Accepted {
        let step = {
            let mut active = self.lock_active();
            self.accept_locked(&mut active, &chunk)
        };
        match step {
            Step::Settle(result) => Accepted::Settled(result),
            Step::Committing(commit) => Accepted::Pending(Box::pin(commit)),
            Step::Wait(commit) => {
                let owner = Arc::clone(self);
                Accepted::Pending(Box::pin(async move {
                    let _ = commit.await;
                    owner.accept(chunk).outcome().await
                }))
            }
        }
    }

    /// Reads counters only: a partial upload is never reopened or rehashed to
    /// answer a status.
    pub fn status(&self, session_id: &str, upload_id: &str) -> AttachmentOperationStatus {
        let active = self.lock_active();
        if let Some(operation) = active.get(&operation_key(session_id, upload_id)) {
            return status_from_journal(&operation.journal);
        }
        match load_attachment_operation(&self.base, session_id, upload_id) {
            AttachmentOperationLoad::Missing => {
                empty_status(upload_id, AttachmentOperationError::UploadNotFound)
            }
            AttachmentOperationLoad::Invalid => {
                empty_status(upload_id, AttachmentOperationError::WriteFailed)
            }
            AttachmentOperationLoad::Loaded { paths, mut journal } => {
                if journal.error.is_empty() && !journal.final_name.is_empty() && !journal.committed
                {
                    recover_finalization(&paths, &mut journal);
                } else if journal.error.is_empty()
                    && journal.final_name.is_empty()
                    && !temp_matches_journal(&paths.temp_path, &journal)
                {
                    fail_journal(&paths, &mut journal, AttachmentOperationError::WriteFailed);
                }
                status_from_journal(&journal)
            }
        }
    }

    /// A dead direct carrier releases its file; its durable status stays
    /// queryable and a new carrier may resume it.
    pub fn detach_direct_carrier(&self, carrier_id: &str) {
        self.lock_active().retain(|_, operation| {
            let detach = operation.journal.carrier == Carrier::Direct
                && operation.journal.carrier_id == carrier_id
                && operation.commit.is_none();
            if detach {
                tracing::info!(request_id = %operation.journal.request_id, bytes = operation.journal.bytes_written, "a direct attachment carrier detached; its operation is parked");
            }
            !detach
        });
    }

    /// A relay's progress is never journaled, so an idle relay operation can
    /// never resume and is failed; an idle direct operation is parked.
    pub fn sweep_idle(&self) {
        let now = (self.clock)();
        let mut active = self.lock_active();
        let idle: Vec<String> = active
            .values()
            .filter(|operation| {
                operation.commit.is_none()
                    && now.saturating_duration_since(operation.last_activity)
                        > ATTACHMENT_OPERATION_IDLE
            })
            .map(|operation| operation.key.clone())
            .collect();
        for key in idle {
            let Some(operation) = active.remove(&key) else {
                continue;
            };
            tracing::warn!(
                request_id = %operation.journal.request_id,
                carrier = operation.journal.carrier.as_str(),
                bytes = operation.journal.bytes_written,
                "attachment_stream_abandoned"
            );
            if operation.journal.carrier == Carrier::Coordinator {
                fail_operation(operation, AttachmentOperationError::UploadNotFound);
            }
        }
    }

    pub(super) fn lock_active(&self) -> MutexGuard<'_, ActiveTable> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn accept_locked(
        self: &Arc<Self>,
        active: &mut ActiveTable,
        chunk: &AttachmentOperationChunk,
    ) -> Step {
        if !valid_chunk(chunk) {
            return Step::Settle(Err(AttachmentOperationError::UploadMismatch));
        }
        if chunk
            .chunk_sha256
            .as_deref()
            .is_some_and(|digest| !is_chunk_sha256(digest))
        {
            return Step::Settle(Err(AttachmentOperationError::ChunkSha256Mismatch));
        }
        let mut operation = match self.prepare(active, chunk) {
            Ok(Ok(operation)) => operation,
            Ok(Err(error)) => return Step::Settle(Err(error)),
            Err(error) => {
                tracing::warn!(request_id = %chunk.descriptor.request_id, %error, "an attachment operation could not be opened");
                return Step::Settle(Err(AttachmentOperationError::WriteFailed));
            }
        };
        match self.advance(&mut operation, chunk) {
            Ok(Advance::Answer(result)) => {
                restore(active, operation);
                Step::Settle(result)
            }
            Ok(Advance::AwaitCommit(commit)) => {
                restore(active, operation);
                Step::Wait(commit)
            }
            Ok(Advance::Fail(error)) => Step::Settle(Err(fail_operation(operation, error))),
            Ok(Advance::Commit) => self.begin_commit(active, operation),
            Err(error) => {
                tracing::warn!(request_id = %chunk.descriptor.request_id, %error, "an attachment chunk could not be written");
                let journal = &operation.journal;
                if !journal.final_name.is_empty() && !journal.committed && journal.error.is_empty()
                {
                    return Step::Settle(Err(AttachmentOperationError::WriteFailed));
                }
                Step::Settle(Err(fail_operation(
                    operation,
                    AttachmentOperationError::WriteFailed,
                )))
            }
        }
    }

    fn advance(
        &self,
        operation: &mut ActiveOperation,
        chunk: &AttachmentOperationChunk,
    ) -> io::Result<Advance> {
        if let Some(commit) = &operation.commit {
            return Ok(Advance::AwaitCommit(commit.clone()));
        }
        operation.last_activity = (self.clock)();
        let journal = &mut operation.journal;
        if !journal.error.is_empty() {
            return Ok(Advance::Answer(Err(journal_error(journal))));
        }
        if !journal.final_name.is_empty()
            && !journal.committed
            && !recover_finalization(&operation.paths, journal)
        {
            return Ok(Advance::Answer(Err(journal_error(journal))));
        }
        let actual = sha256_hex(&chunk.data);
        let claimed = chunk.chunk_sha256.as_deref().unwrap_or(&actual);
        if actual != claimed {
            return Ok(Advance::Fail(AttachmentOperationError::ChunkSha256Mismatch));
        }
        if is_last_accepted_duplicate(journal, chunk, claimed) {
            return Ok(Advance::Answer(Ok(receipt_from_journal(journal))));
        }
        if journal.committed || u64::from(chunk.seq) != journal.next_seq {
            return Ok(Advance::Fail(AttachmentOperationError::ChunkOutOfOrder));
        }
        if chunk.offset != journal.bytes_written {
            return Ok(Advance::Fail(AttachmentOperationError::ChunkOffsetMismatch));
        }
        let length = chunk.data.len() as u64;
        let remaining = |total: u64| total.saturating_sub(journal.bytes_written);
        if journal
            .total_bytes
            .is_some_and(|total| length > remaining(total))
        {
            return Ok(Advance::Fail(AttachmentOperationError::TotalBytesMismatch));
        }
        let file = operation
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("the attachment temp is not open"))?;
        file.write_all(&chunk.data)?;
        operation.hasher.update(&chunk.data);
        journal.next_seq += 1;
        journal.bytes_written += length;
        journal.last_chunk_sha256 = claimed.to_owned();
        journal.last_chunk_final = chunk.last;
        if !chunk.last {
            if chunk.carrier == Carrier::Direct {
                persist_attachment_operation(&operation.paths, journal, false)?;
            }
            return Ok(Advance::Answer(Ok(receipt_from_journal(journal))));
        }
        if journal
            .total_bytes
            .is_some_and(|total| journal.bytes_written != total)
        {
            return Ok(Advance::Fail(AttachmentOperationError::TotalBytesMismatch));
        }
        Ok(Advance::Commit)
    }
}

fn valid_chunk(chunk: &AttachmentOperationChunk) -> bool {
    chunk.offset <= MAX_SAFE_INTEGER
        && chunk
            .descriptor
            .total_bytes
            .is_none_or(|total| total <= MAX_SAFE_INTEGER)
        && chunk.carrier_id.encode_utf16().count() <= MAX_CARRIER_ID_UNITS
}

fn is_last_accepted_duplicate(
    journal: &AttachmentOperationJournal,
    chunk: &AttachmentOperationChunk,
    claimed: &str,
) -> bool {
    journal.next_seq > 0
        && u64::from(chunk.seq) == journal.next_seq - 1
        && chunk.offset.saturating_add(chunk.data.len() as u64) == journal.bytes_written
        && claimed == journal.last_chunk_sha256
        && chunk.last == journal.last_chunk_final
}

pub(super) fn operation_key(session_id: &str, request_id: &str) -> String {
    format!("{session_id}\u{0}{request_id}")
}
