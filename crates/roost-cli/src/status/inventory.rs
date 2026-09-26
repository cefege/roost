//! The read-only coordinator-database read behind `roost status`'s fleet rows.
//! Called by status/collect.rs and nothing else. Opened read-only and against
//! the installed database path, never a copy: the whole value of the readout is
//! that it describes the coordinator that is running right now.
//!
//! A missing or unreadable database is returned as an error, not as an empty
//! roster, because the two mean opposite things. "No workers" is a finding an
//! operator must see; "the database is not there" is a broken readout that
//! would silently claim an empty fleet. The caller decides how loudly to say so.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{AssertSqlSafe, Row, SqlitePool};

use crate::status::report::{WorkerStatus, parse_keeper_runtime, parse_terminal_core_capacity};

/// Interpolated into the `pragma_table_info` query below. A constant of this
/// crate's, never operator input: a table name cannot be a bind parameter.
const WORKERS_TABLE: &str = "workers";
const SESSIONS_TABLE: &str = "sessions";

/// The two projections a coordinator only started writing in a later release.
/// Selecting them unconditionally would make `roost status` fail outright
/// against an older install, so their presence is probed and a missing column
/// is projected as NULL — the same as a row that has never reported one.
const KEEPER_RUNTIME_COLUMN: &str = "keeper_runtime_json";
const CAPACITY_COLUMN: &str = "terminal_core_capacity_json";

#[derive(Debug, thiserror::Error)]
pub enum InventoryError {
    #[error("coordinator database not found: {0}")]
    Missing(PathBuf),
    #[error("coordinator database unreadable: {0}")]
    Unreadable(String),
}

/// One machine's row, with staleness derived from `now_ms`.
pub async fn worker_inventory(
    database_path: &Path,
    now_ms: i64,
) -> Result<Vec<WorkerStatus>, InventoryError> {
    if !database_path.exists() {
        return Err(InventoryError::Missing(database_path.to_path_buf()));
    }
    let pool = open_read_only(database_path).await?;
    let workers = read_workers(&pool, now_ms).await;
    pool.close().await;
    workers
}

async fn open_read_only(database_path: &Path) -> Result<SqlitePool, InventoryError> {
    let options = SqliteConnectOptions::new()
        .filename(database_path)
        .read_only(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|error| InventoryError::Unreadable(error.to_string()))
}

async fn read_workers(pool: &SqlitePool, now_ms: i64) -> Result<Vec<WorkerStatus>, InventoryError> {
    let keeper_projection = projection(pool, KEEPER_RUNTIME_COLUMN).await;
    let capacity_projection = projection(pool, CAPACITY_COLUMN).await;
    let open_sessions = open_session_ids_by_worker(pool).await?;
    let sql = format!(
        "SELECT fp, label, os, reachable_addr, git_sha, \
         {keeper_projection}, {capacity_projection}, last_seen_ms \
         FROM {WORKERS_TABLE} WHERE deleted_at_ms IS NULL"
    );
    let rows = sqlx::query(AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
        .map_err(unreadable)?;
    let mut workers = Vec::with_capacity(rows.len());
    for row in rows {
        let fingerprint: String = text(&row, "fp")?;
        let mut worker = WorkerStatus {
            fingerprint: fingerprint.clone(),
            label: text(&row, "label")?,
            os: text(&row, "os")?,
            reachable_addr: optional_text(&row, "reachable_addr"),
            git_sha: optional_text(&row, "git_sha"),
            keeper_runtime: parse_keeper_runtime(
                optional_text(&row, KEEPER_RUNTIME_COLUMN).as_deref(),
            ),
            terminal_core_capacity: parse_terminal_core_capacity(
                optional_text(&row, CAPACITY_COLUMN).as_deref(),
            ),
            coordinator_open_session_ids: open_sessions
                .get(&fingerprint)
                .cloned()
                .unwrap_or_default(),
            last_seen_ms: row.try_get::<i64, _>("last_seen_ms").map_err(unreadable)?,
            age_ms: 0,
            stale: false,
        };
        worker = worker.with_derived_age(now_ms);
        workers.push(worker);
    }
    Ok(workers)
}

/// The column name, or `NULL AS <name>` when this coordinator predates it.
///
/// The `AssertSqlSafe` wrapper is the audit sqlx asks for, and the audit is
/// this: the only things interpolated into these statements are the two column
/// constants above and the two table constants below. None of them is operator
/// input, and a table or column name cannot be a bind parameter — which is why
/// they are constants rather than arguments.
async fn projection(pool: &SqlitePool, column: &str) -> String {
    let sql = format!(
        "SELECT 1 AS present FROM pragma_table_info('{WORKERS_TABLE}') WHERE name = ? LIMIT 1"
    );
    let found = sqlx::query(AssertSqlSafe(sql))
        .bind(column)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .is_some();
    if found {
        column.to_string()
    } else {
        format!("NULL AS {column}")
    }
}

async fn open_session_ids_by_worker(
    pool: &SqlitePool,
) -> Result<BTreeMap<String, Vec<String>>, InventoryError> {
    let sql = format!("SELECT id, worker_fp FROM {SESSIONS_TABLE} WHERE status = 'open'");
    let rows = sqlx::query(AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
        .map_err(unreadable)?;
    let mut by_worker: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in rows {
        let worker_fp: String = text(&row, "worker_fp")?;
        let id: String = text(&row, "id")?;
        by_worker.entry(worker_fp).or_default().push(id);
    }
    for ids in by_worker.values_mut() {
        ids.sort();
    }
    Ok(by_worker)
}

fn text(row: &SqliteRow, column: &str) -> Result<String, InventoryError> {
    row.try_get::<String, _>(column).map_err(unreadable)
}

fn optional_text(row: &SqliteRow, column: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(column).ok().flatten()
}

fn unreadable(error: sqlx::Error) -> InventoryError {
    InventoryError::Unreadable(error.to_string())
}
