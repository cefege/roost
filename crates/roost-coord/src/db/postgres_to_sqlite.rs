//! Copying a coordinator's Postgres database into a SQLite file: the path back
//! from a stateless install to a file-backed one. Called by `roost
//! db-to-sqlite`; shares `sqlite_to_postgres`'s catalog (one table list for
//! both directions), its error type and its report, and `db::open` migrates
//! both ends to this build's schema.
//!
//! The source is read in one REPEATABLE READ snapshot, so a coordinator still
//! writing it does not tear the copy. The target is written in one SQLite
//! transaction: a failure leaves the file as it was.

use std::path::Path;
use std::time::Instant;

use futures_util::TryStreamExt as _;
use roost_host::DatabaseLocation;
use sqlx::postgres::{PgConnection, PgRow};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{AssertSqlSafe, Connection as _, Row as _, Sqlite};

use super::sqlite_to_postgres::catalog::{
    ColumnKind, TablePlan, quote_identifier, read_postgres_plan, verify_sqlite_matches,
};
use super::sqlite_to_postgres::{ExistingRows, TableTransfer, TransferError, TransferReport};

/// Copy the Postgres database at `source_url` into the SQLite file at
/// `target_path`, creating and migrating the file first.
///
/// The file must not be open in a running coordinator: its writes would race
/// the copy's transaction.
pub async fn copy_postgres_to_sqlite(
    source_url: &str,
    target_path: &Path,
    existing: ExistingRows,
) -> Result<TransferReport, TransferError> {
    let started = Instant::now();
    if let Some(directory) = target_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(directory).map_err(|source| TransferError::TargetDirectory {
            path: directory.to_path_buf(),
            source,
        })?;
    }
    let source = super::open(&DatabaseLocation::Postgres(source_url.to_owned()))
        .await
        .map_err(|source| TransferError::Open {
            side: "Postgres",
            source,
        })?;
    source.pool().close().await;
    let target = super::open(&DatabaseLocation::SqliteFile(target_path.to_path_buf()))
        .await
        .map_err(|source| TransferError::Open {
            side: "SQLite",
            source,
        })?;
    let mut postgres = PgConnection::connect(source_url).await?;
    let plan = read_postgres_plan(&mut postgres).await?;
    verify_sqlite_matches(&target, &plan).await?;
    // The coordinator's handle is a pool over `Any`; the copy wants one native
    // connection whose transaction it owns end to end.
    target.pool().close().await;
    let mut sqlite = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(target_path)
            .foreign_keys(true),
    )
    .await?;

    let mut snapshot = postgres.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *snapshot)
        .await?;
    let mut transaction = sqlite.begin().await?;
    // Checked at commit, so a row may reference one later in its own table.
    sqlx::query("PRAGMA defer_foreign_keys = ON")
        .execute(&mut *transaction)
        .await?;
    prepare_target(&mut transaction, &plan, existing).await?;
    let triggers = drop_triggers(&mut transaction).await?;
    let mut tables = Vec::with_capacity(plan.len());
    for table in &plan {
        let rows = copy_table(&mut snapshot, &mut transaction, table).await?;
        tables.push(TableTransfer {
            table: table.name.clone(),
            rows,
        });
    }
    for trigger in &triggers {
        sqlx::query(AssertSqlSafe(trigger.clone()))
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    snapshot.commit().await?;
    let report = TransferReport {
        tables,
        elapsed: started.elapsed(),
    };
    tracing::info!(
        tables = report.tables.len(),
        rows = report.total_rows(),
        elapsed_ms = u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
        "copied the Postgres coordinator database into SQLite",
    );
    Ok(report)
}

/// Empty, or refuse, a target that already holds rows. Children go first so
/// every delete satisfies the foreign keys the inserts will re-establish.
async fn prepare_target(
    transaction: &mut SqliteConnection,
    plan: &[TablePlan],
    existing: ExistingRows,
) -> Result<(), TransferError> {
    match existing {
        ExistingRows::Replace => {
            for table in plan.iter().rev() {
                sqlx::query(AssertSqlSafe(format!(
                    "DELETE FROM {}",
                    quote_identifier(&table.name)
                )))
                .execute(&mut *transaction)
                .await?;
            }
            tracing::info!(
                tables = plan.len(),
                "emptied the SQLite database before the copy"
            );
        }
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
    Ok(())
}

/// Drop every trigger and return the statements that recreate it.
///
/// SQLite has no `DISABLE TRIGGER`. The triggers re-check invariants the
/// source's identical triggers enforced when each row was first written, and
/// re-firing them on a copy makes insert ORDER part of correctness (a key
/// inserted after its own revocation is refused). Dropping and recreating them
/// inside the copy's transaction rolls back with everything else.
async fn drop_triggers(transaction: &mut SqliteConnection) -> Result<Vec<String>, TransferError> {
    let triggers: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, sql FROM sqlite_master WHERE type = 'trigger' AND sql IS NOT NULL",
    )
    .fetch_all(&mut *transaction)
    .await?;
    for (name, _) in &triggers {
        sqlx::query(AssertSqlSafe(format!(
            "DROP TRIGGER {}",
            quote_identifier(name)
        )))
        .execute(&mut *transaction)
        .await?;
    }
    Ok(triggers.into_iter().map(|(_, sql)| sql).collect())
}

/// Stream one table out of the snapshot, insert each row, and prove the count.
async fn copy_table(
    snapshot: &mut PgConnection,
    target: &mut SqliteConnection,
    table: &TablePlan,
) -> Result<u64, TransferError> {
    let quoted = quote_identifier(&table.name);
    let expected: i64 = sqlx::query_scalar(AssertSqlSafe(format!("SELECT COUNT(*) FROM {quoted}")))
        .fetch_one(&mut *snapshot)
        .await?;
    let expected = u64::try_from(expected).unwrap_or_default();
    let columns = table
        .columns
        .iter()
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = vec!["?"; table.columns.len()].join(", ");
    let insert = format!("INSERT INTO {quoted} ({columns}) VALUES ({placeholders})");
    let select = format!("SELECT {columns} FROM {quoted}");
    let mut rows = sqlx::query(AssertSqlSafe(select)).fetch(&mut *snapshot);
    let mut copied = 0_u64;
    while let Some(row) = rows.try_next().await? {
        insert_row(target, &insert, table, &row).await?;
        copied += 1;
    }
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

async fn insert_row(
    target: &mut SqliteConnection,
    insert: &str,
    table: &TablePlan,
    row: &PgRow,
) -> Result<(), TransferError> {
    let mut statement = sqlx::query::<Sqlite>(AssertSqlSafe(insert.to_owned()));
    for (index, column) in table.columns.iter().enumerate() {
        let value_error = |error: sqlx::Error| TransferError::Value {
            table: table.name.clone(),
            column: column.name.clone(),
            reason: error.to_string(),
        };
        statement = match column.kind {
            ColumnKind::BigInt => {
                statement.bind(row.try_get::<Option<i64>, _>(index).map_err(value_error)?)
            }
            ColumnKind::Text => statement.bind(
                row.try_get::<Option<String>, _>(index)
                    .map_err(value_error)?,
            ),
            ColumnKind::Bytea => statement.bind(
                row.try_get::<Option<Vec<u8>>, _>(index)
                    .map_err(value_error)?,
            ),
        };
    }
    statement.execute(&mut *target).await?;
    Ok(())
}
