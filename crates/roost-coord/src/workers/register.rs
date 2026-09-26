//! The register and rename writes: what a worker just said about itself, and
//! the one label an operator may change.
//!
//! Ported from the `workersRegister` and `workersRename` arms of
//! `apps/coord/src/workers/handlers-workers.ts:87-171`.
//!
//! IDEMPOTENCE IS THE POINT. A worker retries its register after a coordinator
//! restart, and every field it sends is the same, so the second call must leave
//! the row exactly as the first left it -- including `registered_at_ms`, which
//! neither write touches. A register that minted a new enrollment time would
//! reorder the fleet view on every reconnect.
//!
//! EVERY CLAIM IS OPTIONAL AND EVERY ABSENT CLAIM KEEPS THE PRIOR VALUE, with
//! one deliberate exception: `keeper_runtime_json` is cleared on register,
//! because a process that has just started has proved no keeper yet and a stale
//! proof would let an update admission believe it adopted the running one.

use roost_protocol::wire::{HostIdentity, WorkerOs};
use sqlx::AssertSqlSafe;

use crate::db::CoordDb;

use super::rows::{StoredWorkerRow, worker_projection};

/// What a registering worker claimed, after truncation and normalization.
///
/// `Option` is three-state on the wire and two-state here: an absent claim keeps
/// the stored value, and the caller resolves that against the prior row before
/// the write, so the statement itself has no conditional columns to get wrong.
#[derive(Debug, Clone, Default)]
pub struct WorkerClaims {
    /// The operator-facing label.
    pub label: Option<String>,
    /// The platform, already checked against the supported set.
    pub os: Option<WorkerOs>,
    /// The build the worker runs.
    pub git_sha: Option<String>,
    /// The address it resolved on the tailnet.
    pub reachable_addr: Option<String>,
    /// The static machine identity, already normalized.
    pub host_identity: Option<HostIdentity>,
}

/// Why a worker row write failed.
#[derive(Debug, thiserror::Error)]
pub enum WorkerWriteError {
    /// The statement failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    /// The row the update should have matched was gone.
    ///
    /// A tombstone that lands between the read and this write is the only way to
    /// get here, and it is reported rather than papered over with a second read.
    #[error("worker {fp} is no longer registered")]
    Retired {
        /// The worker that vanished.
        fp: String,
    },
    /// A claimed value could not be encoded for the column that stores it.
    #[error("worker {fp}: {reason}")]
    Encoding {
        /// The worker whose claim did not encode.
        fp: String,
        /// What the encoder said.
        reason: String,
    },
}

/// Rewrite a live worker row from what it just claimed.
pub async fn apply_worker_registration(
    database: &CoordDb,
    prior: &StoredWorkerRow,
    claims: &WorkerClaims,
    now_ms: i64,
) -> Result<StoredWorkerRow, WorkerWriteError> {
    let label = claims.label.clone().unwrap_or_else(|| prior.label.clone());
    let os = claims
        .os
        .map_or_else(|| prior.os.clone(), |os| os.as_str().to_owned());
    let git_sha = claims.git_sha.clone().or_else(|| prior.git_sha.clone());
    let reachable_addr = claims
        .reachable_addr
        .clone()
        .or_else(|| prior.reachable_addr.clone());
    let host_identity_json =
        match &claims.host_identity {
            Some(identity) => Some(serde_json::to_string(identity).map_err(|error| {
                WorkerWriteError::Encoding {
                    fp: prior.fp.clone(),
                    reason: error.to_string(),
                }
            })?),
            None => prior.host_identity_json.clone(),
        };
    let sql = format!(
        "UPDATE workers SET label = ?, os = ?, git_sha = ?, reachable_addr = ?, \
         host_identity_json = ?, keeper_runtime_json = NULL, last_seen_ms = ? \
         WHERE fp = ? AND deleted_at_ms IS NULL RETURNING {}",
        worker_projection()
    );
    let updated = sqlx::query_as::<_, StoredWorkerRow>(AssertSqlSafe(sql))
        .bind(label)
        .bind(os)
        .bind(git_sha)
        .bind(reachable_addr)
        .bind(host_identity_json)
        .bind(now_ms)
        .bind(&prior.fp)
        .fetch_optional(database.pool())
        .await?;
    updated.ok_or_else(|| WorkerWriteError::Retired {
        fp: prior.fp.clone(),
    })
}

/// Set the operator-facing label of a live worker.
pub async fn apply_worker_rename(
    database: &CoordDb,
    worker_fp: &str,
    label: &str,
) -> Result<StoredWorkerRow, WorkerWriteError> {
    let sql = format!(
        "UPDATE workers SET label = ? WHERE fp = ? AND deleted_at_ms IS NULL RETURNING {}",
        worker_projection()
    );
    let updated = sqlx::query_as::<_, StoredWorkerRow>(AssertSqlSafe(sql))
        .bind(label)
        .bind(worker_fp)
        .fetch_optional(database.pool())
        .await?;
    updated.ok_or_else(|| WorkerWriteError::Retired {
        fp: worker_fp.to_owned(),
    })
}
