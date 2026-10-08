//! Opening an operation for a chunk: the one already in the table, a journal
//! found on disk (resumed if it is still writable, answered from disk if it is
//! terminal), or a new one at sequence zero. Ports `prepare`, `openLoaded` and
//! the temp checks of v2
//! `apps/worker/src/attachments/attachment-operation-owner.ts`. Called by
//! `operation_owner`.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::file_hash::hash_attachment_file;
use super::journal::{
    AttachmentOperationJournal, AttachmentOperationLoad, AttachmentOperationPaths,
    CreatedOperation, create_attachment_operation, load_attachment_operation,
};
use super::operation_commit::fail_journal;
use super::operation_owner::{
    ActiveOperation, ActiveTable, AttachmentOperationChunk, AttachmentOperationOwner, operation_key,
};
use super::receipts::AttachmentOperationError;

impl AttachmentOperationOwner {
    pub(super) fn prepare(
        &self,
        active: &mut ActiveTable,
        chunk: &AttachmentOperationChunk,
    ) -> io::Result<Result<ActiveOperation, AttachmentOperationError>> {
        let descriptor = &chunk.descriptor;
        let key = operation_key(&descriptor.session_id, &descriptor.request_id);
        if let Some(existing) = active.remove(&key) {
            if !existing.journal.same_descriptor(descriptor)
                || !existing
                    .journal
                    .same_carrier(chunk.carrier, &chunk.carrier_id)
            {
                active.insert(key, existing);
                return Ok(Err(AttachmentOperationError::UploadMismatch));
            }
            return Ok(Ok(existing));
        }
        match load_attachment_operation(&self.base, &descriptor.session_id, &descriptor.request_id)
        {
            AttachmentOperationLoad::Invalid => Ok(Err(AttachmentOperationError::WriteFailed)),
            AttachmentOperationLoad::Missing => {
                if chunk.seq != 0 {
                    return Ok(Err(AttachmentOperationError::ChunkOutOfOrder));
                }
                if chunk.offset != 0 {
                    return Ok(Err(AttachmentOperationError::ChunkOffsetMismatch));
                }
                let created = create_attachment_operation(
                    &self.base,
                    descriptor,
                    chunk.carrier,
                    &chunk.carrier_id,
                )?;
                let Some(CreatedOperation { paths, mut journal }) = created else {
                    return Ok(Err(AttachmentOperationError::UploadNotFound));
                };
                match open_new_temp(&paths.temp_path) {
                    Ok(file) => {
                        tracing::info!(request_id = %journal.request_id, carrier = chunk.carrier.as_str(), "an attachment operation opened");
                        Ok(Ok(self.activate(
                            key,
                            paths,
                            journal,
                            Some(file),
                            Sha256::new(),
                            true,
                        )))
                    }
                    Err(error) => {
                        tracing::warn!(request_id = %journal.request_id, %error, "an attachment temp could not be created");
                        Ok(Err(fail_journal(
                            &paths,
                            &mut journal,
                            AttachmentOperationError::WriteFailed,
                        )))
                    }
                }
            }
            AttachmentOperationLoad::Loaded { paths, journal } => {
                if !journal.same_descriptor(descriptor)
                    || !journal.same_carrier(chunk.carrier, &chunk.carrier_id)
                {
                    return Ok(Err(AttachmentOperationError::UploadMismatch));
                }
                Ok(self.open_loaded(key, paths, *journal))
            }
        }
    }

    fn open_loaded(
        &self,
        key: String,
        paths: AttachmentOperationPaths,
        mut journal: AttachmentOperationJournal,
    ) -> Result<ActiveOperation, AttachmentOperationError> {
        if !journal.error.is_empty() || journal.committed || !journal.final_name.is_empty() {
            return Ok(self.activate(key, paths, journal, None, Sha256::new(), false));
        }
        match resume_temp(&paths.temp_path, &journal) {
            Ok((file, hasher)) => {
                tracing::info!(request_id = %journal.request_id, bytes = journal.bytes_written, "a parked attachment operation resumed");
                Ok(self.activate(key, paths, journal, Some(file), hasher, true))
            }
            Err(error) => {
                tracing::warn!(request_id = %journal.request_id, %error, "a parked attachment operation could not resume");
                Err(fail_journal(
                    &paths,
                    &mut journal,
                    AttachmentOperationError::WriteFailed,
                ))
            }
        }
    }

    fn activate(
        &self,
        key: String,
        paths: AttachmentOperationPaths,
        journal: AttachmentOperationJournal,
        file: Option<File>,
        hasher: Sha256,
        registered: bool,
    ) -> ActiveOperation {
        ActiveOperation {
            key,
            paths,
            journal,
            file,
            hasher,
            last_activity: (self.clock)(),
            commit: None,
            registered,
        }
    }
}

pub(super) fn restore(active: &mut ActiveTable, operation: ActiveOperation) {
    if operation.registered {
        active.insert(operation.key.clone(), operation);
    }
}

fn open_new_temp(path: &Path) -> io::Result<File> {
    roost_keeper::owner_only::create_truncate_private_file(path)
}

/// Reopen a parked temp positioned after the bytes it holds. v2 reopened it
/// `r+` and wrote at offset 0, overwriting the head of every resumed upload;
/// appending is what its journal and digest already assume.
fn resume_temp(
    temp_path: &Path,
    journal: &AttachmentOperationJournal,
) -> io::Result<(File, Sha256)> {
    if !temp_matches_journal(temp_path, journal) {
        return Err(io::Error::other("attachment temp mismatch"));
    }
    let file = OpenOptions::new().append(true).open(temp_path)?;
    Ok((file, hash_attachment_file(temp_path)?))
}

pub(super) fn temp_matches_journal(temp_path: &Path, journal: &AttachmentOperationJournal) -> bool {
    std::fs::metadata(temp_path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == journal.bytes_written)
}
