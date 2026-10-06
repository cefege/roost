//! An operation's final commit and its failure paths: reserve and occupy the
//! final name without yielding, flush the bytes and the name before the
//! committed journal, and finish from the journal a commit a crash interrupted.
//! Ports `commitFinal`, `recoverFinalization` and `failJournal` of v2
//! `apps/worker/src/attachments/attachment-operation-owner.ts`. Called by
//! `operation_owner`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::FutureExt as _;

use super::file_hash::{digest_hex, sha256_attachment_file};
use super::file_store::{
    attachment_reply_path, commit_attachment_destination, place_attachment_destination,
    record_attachment_hash, reserve_attachment_destination, sync_attachment_directory_async,
    sync_attachment_file_async,
};
use super::journal::{
    AttachmentOperationJournal, AttachmentOperationPaths, persist_attachment_operation,
    remove_attachment_temp,
};
use super::operation_owner::{
    ActiveOperation, ActiveTable, AttachmentOperationOwner, SharedCommit, Step,
};
use super::receipts::{AttachmentOperationError, AttachmentOperationResult, receipt_from_journal};

impl AttachmentOperationOwner {
    /// Reservation, journal write and rename run in this call, under the table
    /// lock, so no other upload can reserve the same name before this one
    /// occupies it. The flushes then run on their own task, so the commit
    /// finishes even if nobody awaits it, and every later chunk for this
    /// operation waits on its outcome.
    pub(super) fn begin_commit(
        self: &Arc<Self>,
        active: &mut ActiveTable,
        mut operation: ActiveOperation,
    ) -> Step {
        drop(operation.file.take());
        let destination =
            reserve_attachment_destination(&operation.paths.media_dir, &operation.journal.filename);
        operation.journal.final_name = destination.file_name;
        operation.journal.content_sha256 = digest_hex(std::mem::take(&mut operation.hasher));
        let placed = persist_attachment_operation(&operation.paths, &operation.journal, false)
            .and_then(|()| {
                place_attachment_destination(
                    &operation.paths.media_dir,
                    &operation.paths.temp_path,
                    &operation.journal.final_name,
                    &operation.journal.content_sha256,
                )
            });
        let placed = match placed {
            Ok(placed) => placed,
            Err(error) => return Step::Settle(Err(commit_failed(operation, &error))),
        };
        let owner = Arc::clone(self);
        let key = operation.key.clone();
        let media_dir = operation.paths.media_dir.clone();
        let flush =
            tokio::spawn(
                async move { owner.finish_commit(key, placed.file_path, media_dir).await },
            );
        let commit: SharedCommit = async move {
            flush
                .await
                .unwrap_or(Err(AttachmentOperationError::WriteFailed))
        }
        .boxed()
        .shared();
        operation.commit = Some(commit.clone());
        active.insert(operation.key.clone(), operation);
        Step::Committing(commit)
    }

    async fn finish_commit(
        self: Arc<Self>,
        key: String,
        placed_path: PathBuf,
        media_dir: PathBuf,
    ) -> AttachmentOperationResult {
        let flushed = self.flush_commit(&key, &placed_path, &media_dir).await;
        // Held through the failure path, so a status read cannot find the
        // journal on disk half-failed and try to recover it.
        let mut active = self.lock_active();
        let Some(operation) = active.remove(&key) else {
            tracing::warn!(
                "a committing attachment operation left the table before its commit finished"
            );
            return Err(AttachmentOperationError::WriteFailed);
        };
        match flushed {
            Ok(()) => {
                tracing::info!(
                    session_id = %operation.journal.session_id,
                    size = operation.journal.bytes_written,
                    "attachment_stream_saved"
                );
                Ok(receipt_from_journal(&operation.journal))
            }
            Err(error) => Err(commit_failed(operation, &error)),
        }
    }

    /// Round one makes the bytes and their name durable; only then may round
    /// two make a committed journal durable, so a status never reports bytes
    /// the disk does not hold.
    async fn flush_commit(
        &self,
        key: &str,
        placed_path: &Path,
        media_dir: &Path,
    ) -> io::Result<()> {
        tokio::try_join!(
            sync_attachment_file_async(placed_path),
            sync_attachment_directory_async(media_dir),
        )?;
        let (journal_path, operation_dir) = {
            let mut active = self.lock_active();
            let operation = active
                .get_mut(key)
                .ok_or_else(|| io::Error::other("the committing attachment operation vanished"))?;
            let paths = &operation.paths;
            let journal = &mut operation.journal;
            record_attachment_hash(
                &paths.media_dir,
                &journal.content_sha256,
                &journal.final_name,
            );
            journal.abs_path = reply_path(&paths.media_dir, placed_path, journal.short_path);
            journal.committed = true;
            persist_attachment_operation(paths, journal, false)?;
            (paths.journal_path.clone(), paths.operation_dir.clone())
        };
        tokio::try_join!(
            sync_attachment_file_async(&journal_path),
            sync_attachment_directory_async(&operation_dir),
        )?;
        Ok(())
    }
}

/// Finish a commit whose journal names its final file but was never marked
/// committed: occupy the name (or verify it holds these bytes), flush, and
/// record the outcome. Any failure fails the journal.
pub(super) fn recover_finalization(
    paths: &AttachmentOperationPaths,
    journal: &mut AttachmentOperationJournal,
) -> bool {
    let committed = match commit_attachment_destination(
        &paths.media_dir,
        &paths.temp_path,
        &journal.final_name,
        &journal.content_sha256,
    ) {
        Ok(committed) => committed,
        Err(error) => {
            tracing::warn!(request_id = %journal.request_id, %error, "an interrupted attachment commit could not be finished");
            fail_journal(paths, journal, AttachmentOperationError::WriteFailed);
            return false;
        }
    };
    if !committed.verified_existing_file
        && !sha256_attachment_file(&committed.file_path)
            .is_ok_and(|digest| digest == journal.content_sha256)
    {
        fail_journal(paths, journal, AttachmentOperationError::WriteFailed);
        return false;
    }
    journal.abs_path = reply_path(&paths.media_dir, &committed.file_path, journal.short_path);
    journal.committed = true;
    if let Err(error) = persist_attachment_operation(paths, journal, true) {
        tracing::warn!(request_id = %journal.request_id, %error, "a recovered attachment commit could not be journaled");
        fail_journal(paths, journal, AttachmentOperationError::WriteFailed);
        return false;
    }
    tracing::info!(request_id = %journal.request_id, "an interrupted attachment commit was finished from its journal");
    true
}

/// End an operation: its file closes and its journal records the error.
pub(super) fn fail_operation(
    operation: ActiveOperation,
    error: AttachmentOperationError,
) -> AttachmentOperationError {
    let ActiveOperation {
        paths,
        mut journal,
        file,
        ..
    } = operation;
    drop(file);
    fail_journal(&paths, &mut journal, error)
}

/// Record the error durably and drop the temp. A journal that cannot be
/// written stays uncommitted, which is still a failure to any reader.
pub(super) fn fail_journal(
    paths: &AttachmentOperationPaths,
    journal: &mut AttachmentOperationJournal,
    error: AttachmentOperationError,
) -> AttachmentOperationError {
    journal.error = error.as_str().to_owned();
    if let Err(persist_error) = persist_attachment_operation(paths, journal, true) {
        tracing::warn!(request_id = %journal.request_id, error = %persist_error, "a failed attachment operation's journal could not be written");
    }
    remove_attachment_temp(paths);
    tracing::info!(request_id = %journal.request_id, error = error.as_str(), "an attachment operation failed");
    error
}

/// A commit that reserved its final name but did not finish leaves a
/// recoverable journal: a later status finishes it. Anything else — including
/// a commit whose last flush failed — is failed, never reported as a receipt.
fn commit_failed(mut operation: ActiveOperation, error: &io::Error) -> AttachmentOperationError {
    tracing::warn!(request_id = %operation.journal.request_id, %error, "an attachment commit failed");
    let journal = &operation.journal;
    if !journal.final_name.is_empty() && !journal.committed && journal.error.is_empty() {
        return AttachmentOperationError::WriteFailed;
    }
    operation.journal.committed = false;
    fail_operation(operation, AttachmentOperationError::WriteFailed)
}

fn reply_path(media_dir: &Path, file_path: &Path, short_path: bool) -> String {
    attachment_reply_path(media_dir, file_path, short_path)
        .to_string_lossy()
        .into_owned()
}
