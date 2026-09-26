//! The `workers` row: the columns every worker read needs, and the three reads
//! that find one. The projections live in the sibling `projection`.
//!
//! Ported from the row half of `packages/protocol/src/wire/row-proto.ts` and
//! from the `selectFrom("workers")` reads in
//! `apps/coord/src/workers/handlers-workers*.ts`.
//!
//! `os` IS TEXT, NOT AN ENUM, because a column is text and no `CHECK`
//! constraint decides what this build understands; `worker_os` resolves it
//! through the one platform vocabulary in `roost-platform` rather than a second
//! list of platform names.

use roost_platform::host_platform::HostPlatform;
use roost_protocol::wire::WorkerOs;
use sqlx::{AssertSqlSafe, FromRow};

use crate::db::CoordDb;

/// The `workers` columns every projection reads, in one list so a reader and the
/// projection cannot name different sets.
pub const WORKER_COLUMNS: [&str; 11] = [
    "fp",
    "label",
    "os",
    "git_sha",
    "host_metrics_json",
    "registered_at_ms",
    "last_seen_ms",
    "reachable_addr",
    "keeper_runtime_json",
    "terminal_core_capacity_json",
    "host_identity_json",
];

/// A stored worker row, as the database spells it.
#[derive(Debug, Clone, FromRow)]
pub struct StoredWorkerRow {
    /// The worker's fingerprint.
    pub fp: String,
    /// The operator-facing label.
    pub label: String,
    /// The worker's platform, as text.
    pub os: String,
    /// The build the worker last reported.
    pub git_sha: Option<String>,
    /// The last sampled load, as JSON text.
    pub host_metrics_json: Option<String>,
    /// When the worker enrolled.
    pub registered_at_ms: i64,
    /// When the worker was last heard from.
    pub last_seen_ms: i64,
    /// The tailnet-reachable address the worker resolved.
    pub reachable_addr: Option<String>,
    /// The authenticated keeper proof, as JSON text.
    pub keeper_runtime_json: Option<String>,
    /// The worker's terminal-core admission report, as JSON text.
    pub terminal_core_capacity_json: Option<String>,
    /// The static machine identity, as JSON text.
    pub host_identity_json: Option<String>,
}

/// A tombstoned worker row: the row is still there, and that is the point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerTombstone {
    /// The worker that was deleted.
    pub fp: String,
    /// When it was deleted.
    pub deleted_at_ms: i64,
}

/// Why a worker read failed.
#[derive(Debug, thiserror::Error)]
pub enum WorkerRowError {
    /// The statement failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
}

/// The `SELECT` list every read of a live row shares.
#[must_use]
pub fn worker_projection() -> String {
    WORKER_COLUMNS.join(", ")
}

/// Every worker that is not tombstoned, oldest registration first.
///
/// The order is the renderer's: a fleet view lists the machine that enrolled
/// first first, and a tie has to break the same way in every client or two rows
/// swap places between calls.
pub async fn read_live_workers(database: &CoordDb) -> Result<Vec<StoredWorkerRow>, WorkerRowError> {
    let sql = format!(
        "SELECT {} FROM workers WHERE deleted_at_ms IS NULL ORDER BY registered_at_ms, fp",
        worker_projection()
    );
    let rows = sqlx::query_as::<_, StoredWorkerRow>(AssertSqlSafe(sql))
        .fetch_all(database.pool())
        .await?;
    Ok(rows)
}

/// One live worker row by fingerprint, or `None` for absent and for tombstoned.
pub async fn read_live_worker(
    database: &CoordDb,
    worker_fp: &str,
) -> Result<Option<StoredWorkerRow>, WorkerRowError> {
    let sql = format!(
        "SELECT {} FROM workers WHERE deleted_at_ms IS NULL AND fp = ?",
        worker_projection()
    );
    let row = sqlx::query_as::<_, StoredWorkerRow>(AssertSqlSafe(sql))
        .bind(worker_fp)
        .fetch_optional(database.pool())
        .await?;
    Ok(row)
}

/// The tombstone for a fingerprint, live rows excluded.
///
/// The register and the heartbeat both have to tell "never enrolled" from
/// "deleted by an operator" apart: the second is a fact about a machine that
/// existed, and a refusal that says only "not registered" sends an operator
/// looking for a bootstrap token that was never the problem.
pub async fn read_worker_tombstone(
    database: &CoordDb,
    worker_fp: &str,
) -> Result<Option<WorkerTombstone>, WorkerRowError> {
    let row = sqlx::query_as::<_, (String, Option<i64>)>(
        "SELECT fp, deleted_at_ms FROM workers WHERE fp = ?",
    )
    .bind(worker_fp)
    .fetch_optional(database.pool())
    .await?;
    Ok(row.and_then(|(fp, deleted_at_ms)| {
        deleted_at_ms.map(|deleted_at_ms| WorkerTombstone { fp, deleted_at_ms })
    }))
}

/// The wire platform a stored row names, or `None` when the column holds
/// something this build does not support.
#[must_use]
pub fn worker_os(row: &StoredWorkerRow) -> Option<WorkerOs> {
    match HostPlatform::parse(&row.os) {
        Ok(HostPlatform::MacOs) => Some(WorkerOs::Darwin),
        Ok(HostPlatform::Linux) => Some(WorkerOs::Linux),
        Ok(HostPlatform::Windows) => Some(WorkerOs::Win32),
        Err(_) => None,
    }
}
