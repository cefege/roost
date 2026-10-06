//! The invariants a coordinator SQLite file must hold around its migrations:
//! SQLite enforces foreign keys, no row violates one, and a freshly migrated
//! file passes `PRAGMA integrity_check`.
//!
//! Owned by `crate::db`; `db::open` is the only caller, before and after the
//! migration set runs. Ports `apps/coord/src/db/migration-validation.ts`.
//! Postgres enforces foreign keys unconditionally and has no file to check, so
//! every function here returns `Ok(())` on that backend.

use sqlx::Row as _;

use super::{CoordDb, DbBackend, DbError};

/// Whether `database` is SQLite; on Postgres, logs which check was skipped.
fn applies_to(database: &CoordDb, check: &'static str) -> bool {
    if database.backend() == DbBackend::Sqlite {
        return true;
    }
    tracing::debug!(
        backend = database.backend().label(),
        check,
        "migration validation skipped: the server enforces it"
    );
    false
}

fn database_label(database: &CoordDb) -> String {
    database
        .sqlite_path()
        .map_or_else(String::new, |path| path.display().to_string())
}

/// Turn foreign-key enforcement on and refuse a connection that did not keep it.
///
/// v2's `enableAndVerifyForeignKeys`. Setting it again rather than trusting the
/// connect options matters after a migration: a table rebuild is written with
/// `PRAGMA foreign_keys = OFF`, and a connection a migration left unenforced
/// would accept every orphan the rest of the process writes. A SQLite built
/// without foreign-key support answers no row at all, which is refused too.
pub(super) async fn enable_and_verify_foreign_keys(database: &CoordDb) -> Result<(), DbError> {
    if !applies_to(database, "foreign_keys") {
        return Ok(());
    }
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(database.pool())
        .await?;
    let enforced: Option<(i64,)> = sqlx::query_as("PRAGMA foreign_keys")
        .fetch_optional(database.pool())
        .await?;
    if enforced.map(|(value,)| value) != Some(1) {
        tracing::error!(path = %database_label(database), "database refused: foreign keys unenforced");
        return Err(DbError::ForeignKeysUnenforced);
    }
    Ok(())
}

/// Refuse a database in which any row violates a foreign key.
///
/// v2's `validateForeignKeys` names the first violation as `table -> parent`;
/// this names every distinct pair, so an operator repairing the file sees the
/// whole job rather than one table per restart.
pub(super) async fn validate_foreign_keys(database: &CoordDb) -> Result<(), DbError> {
    if !applies_to(database, "foreign_key_check") {
        return Ok(());
    }
    let rows = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(database.pool())
        .await?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut pairs: Vec<String> = Vec::new();
    for row in &rows {
        let table: String = row.try_get("table")?;
        let parent: String = row.try_get("parent")?;
        let pair = format!("{table} -> {parent}");
        if !pairs.contains(&pair) {
            pairs.push(pair);
        }
    }
    let violations = pairs.join(", ");
    tracing::error!(
        path = %database_label(database),
        rows = rows.len(),
        violations = %violations,
        "database refused: foreign key check failed"
    );
    Err(DbError::ForeignKeyCheck {
        rows: rows.len(),
        violations,
    })
}

/// Refuse a freshly migrated file that SQLite itself reports as corrupt.
///
/// v2's `validateIntegrity`, which its runner applies only after the final
/// pending migration: a full check is a scan of the whole file, and a boot
/// that migrated nothing changed nothing it could have corrupted.
pub(super) async fn validate_integrity(database: &CoordDb, migration: &str) -> Result<(), DbError> {
    if !applies_to(database, "integrity_check") {
        return Ok(());
    }
    let rows: Vec<(String,)> = sqlx::query_as("PRAGMA integrity_check")
        .fetch_all(database.pool())
        .await?;
    if let [(verdict,)] = rows.as_slice()
        && verdict == "ok"
    {
        return Ok(());
    }
    let reason = rows
        .first()
        .map_or_else(|| "no result".to_string(), |(verdict,)| verdict.clone());
    tracing::error!(migration, reason = %reason, "database refused: integrity check failed");
    Err(DbError::MigrationFailed {
        name: migration.to_string(),
        reason: format!("integrity check failed: {reason}"),
    })
}
