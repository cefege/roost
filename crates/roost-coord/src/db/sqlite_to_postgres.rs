//! Copying a coordinator's SQLite file into a Postgres database: the one path
//! from a file-backed install to a stateless one. Called by `roost
//! db-to-postgres`; depends on `db::open` to migrate both ends to this build's
//! schema, on `catalog` for what to copy in which order, and on `batch` for the
//! batched inserts.
//!
//! ONE TRANSACTION. Every row lands, every identity sequence moves past the
//! copied ids, and every table's count is checked against the source before
//! the commit; a failure anywhere leaves the target exactly as it was.

mod batch;
pub mod catalog;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::TryStreamExt as _;
use roost_host::DatabaseLocation;
use sqlx::postgres::PgConnection;
use sqlx::{AssertSqlSafe, Connection as _};

use self::batch::{TableBatch, source_select};
use self::catalog::{TablePlan, quote_identifier, read_target_plan, verify_source_matches};
use super::{CoordDb, DbError};

/// Rows per `INSERT … UNNEST` statement: large enough that a round trip per
/// batch is noise, small enough that one batch of audit rows stays a few MiB.
pub const TRANSFER_BATCH_ROWS: usize = 2_000;

/// What to do with a target that already holds rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingRows {
    /// Refuse: a coordinator already booted against it, or an earlier copy ran.
    Refuse,
    /// Empty every table first, in the same transaction as the copy.
    Replace,
}

/// Why a copy did not happen. Nothing was written to the target in any case.
#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    /// The source file is not there; opening it would create an empty one.
    #[error("the SQLite database {} does not exist", .0.display())]
    SourceMissing(PathBuf),
    /// One end could not be opened or migrated.
    #[error("the {side} database could not be opened: {source}")]
    Open {
        side: &'static str,
        #[source]
        source: DbError,
    },
    /// A statement failed.
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    /// The target holds rows and the caller did not ask to replace them.
    #[error(
        "the Postgres database already holds rows in {tables}; point at an empty database, or \
         replace its contents deliberately"
    )]
    TargetNotEmpty { tables: String },
    /// The two migrated schemas disagree, or use a type this copy cannot carry.
    #[error("the SQLite and Postgres schemas disagree: {0}")]
    SchemaMismatch(String),
    /// A source value does not decode as its target column's type.
    #[error("{table}.{column} holds a value Postgres cannot take: {reason}")]
    Value {
        table: String,
        column: String,
        reason: String,
    },
    /// A table's copied count differs from its source count.
    #[error("{table}: copied {copied} rows but the SQLite file holds {expected}")]
    CountMismatch {
        table: String,
        copied: u64,
        expected: u64,
    },
}

/// One copied table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableTransfer {
    /// The table.
    pub table: String,
    /// Rows copied, equal to the source's count.
    pub rows: u64,
}

/// A finished copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferReport {
    /// Every table, in the order it was copied.
    pub tables: Vec<TableTransfer>,
    /// Wall time from opening the source to the commit.
    pub elapsed: Duration,
}

impl TransferReport {
    /// Rows copied across every table.
    #[must_use]
    pub fn total_rows(&self) -> u64 {
        self.tables.iter().map(|table| table.rows).sum()
    }
}

/// Copy the SQLite file at `source_path` into the Postgres database at
/// `target_url`, migrating both to this build's schema first.
///
/// The source must not be open in a running coordinator: a write landing
/// mid-copy would be missing from the target.
pub async fn copy_sqlite_to_postgres(
    source_path: &Path,
    target_url: &str,
    existing: ExistingRows,
) -> Result<TransferReport, TransferError> {
    let started = Instant::now();
    if !source_path.is_file() {
        return Err(TransferError::SourceMissing(source_path.to_path_buf()));
    }
    let source = super::open(&DatabaseLocation::SqliteFile(source_path.to_path_buf()))
        .await
        .map_err(|source| TransferError::Open {
            side: "SQLite",
            source,
        })?;
    let target = super::open(&DatabaseLocation::Postgres(target_url.to_owned()))
        .await
        .map_err(|source| TransferError::Open {
            side: "Postgres",
            source,
        })?;
    // The coordinator's own handle stays a pool of one over `Any`; the copy
    // wants the native driver's typed array binds, on a connection of its own.
    target.pool().close().await;
    let mut connection = PgConnection::connect(target_url).await?;
    let plan = read_target_plan(&mut connection).await?;
    verify_source_matches(&source, &plan).await?;

    let mut transaction = connection.begin().await?;
    prepare_target(&mut transaction, &plan, existing).await?;
    let mut tables = Vec::with_capacity(plan.len());
    for table in &plan {
        let rows = copy_table(&source, &mut transaction, table).await?;
        tables.push(TableTransfer {
            table: table.name.clone(),
            rows,
        });
    }
    for table in &plan {
        sqlx::query(AssertSqlSafe(format!(
            "ALTER TABLE {} ENABLE TRIGGER USER",
            quote_identifier(&table.name)
        )))
        .execute(&mut *transaction)
        .await?;
        advance_identities(&mut transaction, table).await?;
    }
    transaction.commit().await?;
    source.pool().close().await;
    let report = TransferReport {
        tables,
        elapsed: started.elapsed(),
    };
    tracing::info!(
        tables = report.tables.len(),
        rows = report.total_rows(),
        elapsed_ms = u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
        "copied the SQLite coordinator database into Postgres",
    );
    Ok(report)
}

/// Empty or refuse a non-empty target, then switch off the user triggers.
///
/// The triggers re-check invariants the SQLite file's identical triggers
/// already enforced when each row was first written; re-firing them on a copy
/// would make the insert ORDER part of correctness (a key inserted after its
/// own revocation is refused). Foreign keys stay on: they are checked, and the
/// plan's order satisfies them. `ALTER TABLE` is transactional, so a failed
/// copy rolls the triggers back on with everything else.
async fn prepare_target(
    transaction: &mut PgConnection,
    plan: &[TablePlan],
    existing: ExistingRows,
) -> Result<(), TransferError> {
    let all_tables: Vec<String> = plan
        .iter()
        .map(|table| quote_identifier(&table.name))
        .collect();
    match existing {
        ExistingRows::Replace if !all_tables.is_empty() => {
            sqlx::query(AssertSqlSafe(format!(
                "TRUNCATE {} RESTART IDENTITY CASCADE",
                all_tables.join(", ")
            )))
            .execute(&mut *transaction)
            .await?;
            tracing::info!(
                tables = plan.len(),
                "emptied the Postgres database before the copy"
            );
        }
        ExistingRows::Replace => {}
        ExistingRows::Refuse => {
            let mut occupied = Vec::new();
            for table in plan {
                let has_rows: bool = sqlx::query_scalar(AssertSqlSafe(format!(
                    "SELECT EXISTS (SELECT 1 FROM {})",
                    quote_identifier(&table.name)
                )))
                .fetch_one(&mut *transaction)
                .await?;
                if has_rows {
                    occupied.push(table.name.clone());
                }
            }
            if !occupied.is_empty() {
                return Err(TransferError::TargetNotEmpty {
                    tables: occupied.join(", "),
                });
            }
        }
    }
    for table in &all_tables {
        sqlx::query(AssertSqlSafe(format!(
            "ALTER TABLE {table} DISABLE TRIGGER USER"
        )))
        .execute(&mut *transaction)
        .await?;
    }
    Ok(())
}

/// Stream one table out of SQLite in batches and prove the count.
async fn copy_table(
    source: &CoordDb,
    target: &mut PgConnection,
    table: &TablePlan,
) -> Result<u64, TransferError> {
    let expected: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM {}",
        quote_identifier(&table.name)
    )))
    .fetch_one(source.pool())
    .await?;
    let expected = u64::try_from(expected).unwrap_or_default();
    let mut rows = sqlx::query(AssertSqlSafe(source_select(table))).fetch(source.pool());
    let mut batch = TableBatch::new(table, TRANSFER_BATCH_ROWS);
    let mut copied = 0_u64;
    while let Some(row) = rows.try_next().await? {
        batch.push(&row)?;
        if batch.pending_rows() >= TRANSFER_BATCH_ROWS {
            copied += batch.flush(target).await?;
        }
    }
    copied += batch.flush(target).await?;
    if copied != expected {
        return Err(TransferError::CountMismatch {
            table: table.name.clone(),
            copied,
            expected,
        });
    }
    tracing::debug!(table = %table.name, rows = copied, "copied table");
    Ok(copied)
}

/// Move each identity sequence past the largest copied id, so the next row
/// the coordinator writes does not collide with a copied one.
async fn advance_identities(
    target: &mut PgConnection,
    table: &TablePlan,
) -> Result<(), TransferError> {
    for column in table.columns.iter().filter(|column| column.identity) {
        sqlx::query(AssertSqlSafe(format!(
            "SELECT setval(pg_get_serial_sequence($1, $2), \
             COALESCE((SELECT MAX({column}) FROM {table}), 0) + 1, false)",
            column = quote_identifier(&column.name),
            table = quote_identifier(&table.name),
        )))
        .bind(quote_identifier(&table.name))
        .bind(&column.name)
        .execute(&mut *target)
        .await?;
    }
    Ok(())
}
