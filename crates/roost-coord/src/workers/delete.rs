//! The delete path: one transaction that makes the revocation irreversible, and
//! a chain of best-effort cleanups that cannot undo it.
//!
//! Ported from the `workersDelete` arm of
//! `apps/coord/src/workers/handlers-workers.ts:173-274`.
//!
//! THE COMMIT IS THE POINT, AND EVERYTHING AFTER IT IS VOLATILE. The
//! transaction tombstones the row, revokes the credential, drops the keys and
//! the unused tokens this worker minted. Nothing after that line can be rolled
//! back, which is why the fence that stops the generation runs FIRST and
//! synchronously -- a browser command that reached a deleted worker between the
//! commit and the fence is a command a removed machine executed.
//!
//! THE TWO ROUTE CLEANUPS ARE SEPARATE STEPS, NOT ONE. `retire_worker_routes`
//! answers to the byte hub and `notify_worker_retired` to the view hub: separate
//! objects, separate lifetimes, which merely happen to be called back to back.
//! Folding them into one would make the view hub's failure the byte hub's, and a
//! view hub that throws must not leave a live route pointing at a dead worker.

use std::panic::AssertUnwindSafe;

use roost_protocol::wire::{SessionId, WorkerFp};
use sqlx::SqliteConnection;

use crate::db::CoordDb;

/// The reason recorded against a credential a worker delete revoked.
pub const DELETE_REVOCATION_REASON: &str = "worker-deleted";

/// Why a worker delete failed.
#[derive(Debug, thiserror::Error)]
pub enum WorkerDeleteError {
    /// The statement failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    /// No live row carries that fingerprint.
    #[error("worker not found")]
    NotFound,
    /// The tombstone update matched no row, so the transaction is rolled back
    /// rather than committing a revocation with no tombstone behind it.
    #[error("worker tombstone update lost")]
    TombstoneLost,
}

/// What the delete transaction committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerDeletion {
    /// The sessions this worker owned, from the durable rows. They are NOT
    /// deleted here: a session row is history, and the terminal domains decide
    /// what a dead worker's sessions become.
    pub persisted_session_ids: Vec<String>,
}

/// Tombstone a worker and revoke everything that could authenticate as it.
///
/// One transaction for all of it, because a revocation without a tombstone (or
/// the reverse) is a state no other code path can reach: a worker that is gone
/// from the fleet view but still holds a live credential, or one that is
/// tombstoned while its key still passes the interceptor.
pub async fn delete_worker(
    database: &CoordDb,
    worker_fp: &WorkerFp,
    revoked_by_fp: &str,
    now_ms: i64,
) -> Result<WorkerDeletion, WorkerDeleteError> {
    let mut transaction = database.pool().begin().await?;
    let committed =
        commit_deletion(&mut transaction, worker_fp.as_str(), revoked_by_fp, now_ms).await;
    let deletion = match committed {
        Ok(deletion) => deletion,
        Err(error) => {
            transaction.rollback().await?;
            return Err(error);
        }
    };
    transaction.commit().await?;
    Ok(deletion)
}

/// The transaction body, on its own connection so the rollback is the caller's.
async fn commit_deletion(
    transaction: &mut SqliteConnection,
    worker_fp: &str,
    revoked_by_fp: &str,
    now_ms: i64,
) -> Result<WorkerDeletion, WorkerDeleteError> {
    let live: Option<String> =
        sqlx::query_scalar("SELECT fp FROM workers WHERE fp = ? AND deleted_at_ms IS NULL")
            .bind(worker_fp)
            .fetch_optional(&mut *transaction)
            .await?;
    if live.is_none() {
        return Err(WorkerDeleteError::NotFound);
    }
    let session_ids = sqlx::query_scalar::<_, String>(
        "SELECT id FROM sessions WHERE worker_fp = ? ORDER BY created_at, id",
    )
    .bind(worker_fp)
    .fetch_all(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO authorized_key_revocations (fingerprint, revoked_at_ms, revoked_by_fp, reason) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(worker_fp)
    .bind(now_ms)
    .bind(revoked_by_fp)
    .bind(DELETE_REVOCATION_REASON)
    .execute(&mut *transaction)
    .await?;
    let tombstoned = sqlx::query_scalar::<_, String>(
        "UPDATE workers SET deleted_at_ms = ? WHERE fp = ? AND deleted_at_ms IS NULL RETURNING fp",
    )
    .bind(now_ms)
    .bind(worker_fp)
    .fetch_optional(&mut *transaction)
    .await?;
    if tombstoned.is_none() {
        return Err(WorkerDeleteError::TombstoneLost);
    }
    // An unused token this worker minted would let the machine re-enrol itself
    // through a credential nobody is looking at any more.
    sqlx::query("DELETE FROM bootstrap_tokens WHERE used_at_ms IS NULL AND minted_by_fp = ?")
        .bind(worker_fp)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM authorized_keys WHERE fingerprint = ?")
        .bind(worker_fp)
        .execute(&mut *transaction)
        .await?;
    Ok(WorkerDeletion {
        persisted_session_ids: session_ids,
    })
}

/// Run one post-commit cleanup, isolating its failure from the retirement.
///
/// The seams these steps call return unit, so the only way a collaborator can
/// fail is by panicking; v2's `try { … } catch` around a thrown JS error is
/// this. It matters because each step below is owned by a different object, and
/// one owner throwing must not leave the others' state half-retired.
pub fn best_effort_cleanup(step: &str, worker_fp: &WorkerFp, work: impl FnOnce()) {
    if std::panic::catch_unwind(AssertUnwindSafe(work)).is_err() {
        tracing::warn!(%worker_fp, step, "a post-delete cleanup failed");
    }
}

/// The sessions a retirement released, deduplicated against the durable ones.
///
/// The union is what the view hub is told about: a session can lose a live route
/// without having a durable row yet, and a session with a durable row can have
/// lost its route hours ago.
#[must_use]
pub fn released_sessions(persisted: &[String], volatile: &[SessionId]) -> Vec<SessionId> {
    let mut released: Vec<SessionId> = Vec::with_capacity(persisted.len() + volatile.len());
    for session_id in persisted {
        if let Ok(session_id) = SessionId::try_from(session_id.as_str()) {
            if !released.contains(&session_id) {
                released.push(session_id);
            }
        }
    }
    for session_id in volatile {
        if !released.contains(session_id) {
            released.push(session_id.clone());
        }
    }
    released
}
