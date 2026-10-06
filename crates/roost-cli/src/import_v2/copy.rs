//! Reading a v2 database and writing it into this install's own. Called by
//! `import_v2::mod`; depends on `roost-coord` for the target's open and its
//! single-tenant invariant, on `sqlx` for SQLite, and on the `plan` sibling
//! for every statement it runs.
//!
//! **THE COPY IS SQLITE'S, NOT OURS.** The v2 database is ATTACHed to the
//! connection that owns the v3 one, and each table is copied by a single
//! `INSERT … SELECT`. That is chosen over reading rows out and writing them
//! back for one reason: a value copied by hand is a value marshalled through
//! whatever types the reader happened to handle, so a column whose type nobody
//! thought about — a BLOB public key, a NULL, a wide integer — is either
//! silently coerced or refused. SQLite copying SQLite is the one path where
//! "verbatim" is a property of the operation rather than a claim about it.
//!
//! **ONE TRANSACTION, AND THE READ IS PART OF IT.** Every table is copied
//! inside a single transaction that also holds the read of the v2 database, so
//! the whole import sees one consistent snapshot of a file that is still being
//! written to by the v2 coordinator this install replaces. A v2 database
//! mid-WAL cannot tear across the import: a device paired during the read is
//! either in the snapshot or not in it at all.
//!
//! Every statement here is built by `plan`, from three kinds of thing and no
//! others: a table name from `plan::TABLES`, a column name from the TARGET's
//! own `pragma_table_info`, and a fixed predicate. None of them is operator
//! input — `--from` contributes a filesystem path, which reaches SQL only
//! through the ATTACH URI, and every value a row carries travels as data
//! inside SQLite's own `INSERT … SELECT` rather than as text in a statement.
//! `AssertSqlSafe` is the audit sqlx asks a caller to make in exactly this
//! situation, and this paragraph is the audit.

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::{Acquire, AnyPool, AssertSqlSafe, Database, Executor, IntoArguments, Transaction};

use crate::command_error::CommandFailure;
use crate::import_v2::plan::{
    ImportMode, ModeRefusal, SOURCE_SCHEMA, TABLES, already_present_sql, columns_sql, copy_sql,
    decide_mode, revocation_deletes, source_accounts_sql, source_count_sql,
};

/// How many rows one table contributed, as the operator's report states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableReport {
    /// The table, named as the plan names it.
    pub table: &'static str,
    /// Rows this run wrote.
    pub copied: i64,
    /// Rows the target already had.
    pub already_present: i64,
}

impl TableReport {
    /// The line stdout carries, which is the answer to "what happened".
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "{}: {} copied, {} already present",
            self.table, self.copied, self.already_present
        )
    }
}

/// The source's account id, which is the identity everything else hangs off.
pub async fn source_account(pool: &SqlitePool) -> Result<String, CommandFailure> {
    let sql = source_accounts_sql();
    let ids: Vec<String> = sqlx::query_scalar(AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
        .map_err(|error| read_failure(sql, &error))?;
    match ids.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(CommandFailure::generic(
            "the v2 database holds no account, so there is no identity to carry across. A v2 \
             install always has exactly one; this file is not one.",
        )),
        many => Err(CommandFailure::generic(format!(
            "the v2 database holds {} accounts. This command imports a single self-hosted \
             install, and a v2 database with several is not one.",
            many.len()
        ))),
    }
}

/// What a dry run would report, computed without writing anything.
///
/// The numbers are the ones a real run produces rather than an estimate of
/// them: a run inserts exactly the rows the target does not have, so "copied"
/// is the source's count minus the count already present.
/// `tests/import_v2_copy.rs` runs both and asserts the two reports are equal,
/// which is the only thing that makes a dry run worth reading.
pub async fn estimate(source: &Path, target: &Path) -> Result<Vec<TableReport>, CommandFailure> {
    // A target with no `accounts` table is a target with NOTHING in it, whether
    // the file is absent or merely not yet a v3 database. Both report the
    // first-run numbers, and both must not be opened through the coordinator's
    // own opener to find that out: that opener runs migrations, so a dry run
    // that used it to inspect an unmigrated file would MIGRATE the file it
    // promised not to touch, on the machine where the operator ran `--dry-run`
    // precisely to look before leaping.
    if !target.is_file() || !target_is_migrated(source, target).await? {
        // Nothing to compare against, and nothing to create: a dry run that
        // made the file it is reporting on would not be a dry run.
        let source_pool = open_source_read_only(source).await?;
        let mut reports = Vec::with_capacity(TABLES.len());
        for spec in TABLES {
            let sql = source_count_sql(spec.table, spec.selection);
            let total: i64 = sqlx::query_scalar(AssertSqlSafe(sql.as_str()))
                .fetch_one(&source_pool)
                .await
                .map_err(|error| read_failure(&sql, &error))?;
            reports.push(TableReport {
                table: spec.table,
                copied: total,
                already_present: 0,
            });
        }
        source_pool.close().await;
        return Ok(reports);
    }
    let pool = attached_read_only(source, target).await?;
    let mut reports = Vec::with_capacity(TABLES.len());
    for spec in TABLES {
        let present_sql = already_present_sql(spec.table, spec.selection);
        let total_sql = source_count_sql(spec.table, spec.selection);
        let already: i64 = sqlx::query_scalar(AssertSqlSafe(present_sql.as_str()))
            .fetch_one(&pool)
            .await
            .map_err(|error| read_failure(&present_sql, &error))?;
        let all: i64 = sqlx::query_scalar(AssertSqlSafe(total_sql.as_str()))
            .fetch_one(&pool)
            .await
            .map_err(|error| read_failure(&total_sql, &error))?;
        reports.push(TableReport {
            table: spec.table,
            copied: (all - already).max(0),
            already_present: already,
        });
    }
    pool.close().await;
    Ok(reports)
}

/// Copy every table into `pool`, in the plan's order, in one transaction.
///
/// `pool` is the v3 coordinator's own handle, already migrated, with the v2
/// database attached as `src`. The account ids are read HERE rather than
/// passed in, so the mode is decided against the same transaction that acts on
/// it: a target that changed between the decision and the write would
/// otherwise be written on the strength of a stale answer.
pub async fn apply(
    pool: &AnyPool,
    imported_account: &str,
) -> Result<(ImportMode, Vec<TableReport>), CommandFailure> {
    let mut connection = pool.acquire().await.map_err(|error| {
        CommandFailure::generic(format!("the v3 database could not be reached: {error}"))
    })?;
    let mut transaction = connection
        .begin()
        .await
        .map_err(|error| CommandFailure::generic(format!("the import could not begin: {error}")))?;

    let installed = installed_accounts(&mut transaction).await?;
    let mode =
        decide_mode(&installed, imported_account).map_err(|refusal| refusal_message(&refusal))?;

    let mut reports = Vec::with_capacity(TABLES.len());
    for spec in TABLES {
        let columns = target_columns(&mut transaction, spec.table).await?;
        let copy = copy_sql(spec.table, &columns, spec.selection);
        let total_sql = source_count_sql(spec.table, spec.selection);
        let total: i64 = sqlx::query_scalar(AssertSqlSafe(total_sql.as_str()))
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| read_failure(&total_sql, &error))?;
        let inserted = sqlx::query(AssertSqlSafe(copy.as_str()))
            .execute(&mut *transaction)
            .await
            .map_err(|error| read_failure(&copy, &error))?
            .rows_affected() as i64;
        reports.push(TableReport {
            table: spec.table,
            copied: inserted,
            already_present: (total - inserted).max(0),
        });
    }
    for delete in revocation_deletes() {
        sqlx::query(AssertSqlSafe(*delete))
            .execute(&mut *transaction)
            .await
            .map_err(|error| read_failure(delete, &error))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| CommandFailure::generic(format!("the import did not commit: {error}")))?;
    Ok((mode, reports))
}

/// ATTACH the v2 database to `pool` as `src`, read-only.
///
/// The read-only part is expressed in the filename as `mode=ro` rather than
/// left to the operator's permissions, because this runs as the account that
/// owns the v2 file: without it a bug in the statement list would write to a
/// database belonging to the product being replaced, and the v2 coordinator is
/// live on this same host during the cutover.
pub async fn attach<'c, E>(executor: E, source: &Path) -> Result<(), CommandFailure>
where
    E: Executor<'c>,
    <E::Database as Database>::Arguments: IntoArguments<E::Database>,
{
    let sql = format!(
        "ATTACH DATABASE '{}' AS {SOURCE_SCHEMA}",
        read_only_uri(source).replace('\'', "''")
    );
    sqlx::query::<E::Database>(AssertSqlSafe(sql))
        .execute(executor)
        .await
        .map_err(|error| {
            CommandFailure::generic(format!(
                "the v2 database {} could not be attached: {error}",
                source.display()
            ))
        })?;
    Ok(())
}

/// The `file:` URI that makes an ATTACH read-only.
#[must_use]
pub fn read_only_uri(path: &Path) -> String {
    format!("file:{}?mode=ro", path.display())
}

/// The v2 database behind an ATTACH, for a caller that needs to read it
/// without a target to attach it to.
///
/// The connection's own `main` is empty on purpose: every read of the v2
/// database in this module is spelled `src.<table>`, and a pool where the
/// source was `main` would mean two spellings for one thing — the read that
/// happens before the target exists and the read that happens after it attached.
/// Two spellings is how a dry run's numbers and a real run's numbers drift.
pub async fn open_source_read_only(path: &Path) -> Result<SqlitePool, CommandFailure> {
    let options = SqliteConnectOptions::new()
        .filename(":memory:")
        .foreign_keys(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|error| {
            CommandFailure::generic(format!(
                "the v2 source {} could not be opened: {error}",
                path.display()
            ))
        })?;
    attach(&pool, path).await?;
    Ok(pool)
}

/// Whether the target is already a v3 database, read without writing to it.
///
/// The table is asked for through a read-only handle rather than by opening the
/// file, because "does this database have a schema" is a question a dry run must
/// be able to ask without touching the answer.
async fn target_is_migrated(source: &Path, target: &Path) -> Result<bool, CommandFailure> {
    let pool = attached_read_only(source, target).await?;
    let present: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM main.sqlite_master WHERE type = 'table' AND name = 'accounts'",
    )
    .fetch_one(&pool)
    .await
    .map_err(|error| read_failure("sqlite_master", &error))?;
    pool.close().await;
    Ok(present == 1)
}

/// A read-only view of both databases: the target as `main`, the v2 one as
/// `src`, with neither writable. This is what a dry run reports from.
async fn attached_read_only(source: &Path, target: &Path) -> Result<SqlitePool, CommandFailure> {
    let options = SqliteConnectOptions::new()
        .filename(target)
        .read_only(true)
        .foreign_keys(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|error| {
            CommandFailure::generic(format!(
                "the v3 database {} could not be opened: {error}",
                target.display()
            ))
        })?;
    attach(&pool, source).await?;
    Ok(pool)
}

async fn installed_accounts(
    transaction: &mut Transaction<'_, sqlx::Any>,
) -> Result<Vec<String>, CommandFailure> {
    let sql = "SELECT id FROM main.accounts";
    sqlx::query_scalar(sql)
        .fetch_all(&mut **transaction)
        .await
        .map_err(|error| read_failure(sql, &error))
}

async fn target_columns(
    transaction: &mut Transaction<'_, sqlx::Any>,
    table: &str,
) -> Result<Vec<String>, CommandFailure> {
    let sql = columns_sql(table);
    sqlx::query_scalar(AssertSqlSafe(sql.as_str()))
        .fetch_all(&mut **transaction)
        .await
        .map_err(|error| read_failure(&sql, &error))
}

fn refusal_message(refusal: &ModeRefusal) -> CommandFailure {
    match refusal {
        ModeRefusal::DifferentInstall {
            installed,
            imported,
        } => CommandFailure::usage(format!(
            "this v3 database belongs to a different install: it holds account {installed}, and \
             the database you asked to import from holds {imported}. Importing one install's \
             identity into another's would rewrite the account every other row refers to, so \
             this is refused rather than merged. Point ROOST_COORD_DATA_DIR at this install's \
             data directory, or import into a machine that has not been installed yet."
        )),
        ModeRefusal::NotSelfHosted { accounts } => CommandFailure::usage(format!(
            "the v3 database holds {accounts} accounts. A v3 install is a single self-hosted \
             account, so this is not an install this command can bring up to date."
        )),
    }
}

fn read_failure(sql: &str, error: &sqlx::Error) -> CommandFailure {
    CommandFailure::generic(format!("{sql} failed: {error}"))
}
