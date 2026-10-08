//! Opening the coordinator's database — a SQLite file it owns or a Postgres
//! server someone else runs — and migrating it before anything reads it.
//!
//! Owned by the coordinator. Everything above this file takes a `CoordDb` and
//! never opens a connection, so the connection setup and the migration are
//! stated in exactly one place; `db/backend.rs` holds the per-backend half.
//! Every statement in this crate is written once, in the dialect both backends
//! accept (`$n` placeholders, no SQLite-only functions); the few that cannot be
//! — `PRAGMA`, `VACUUM`, the backup and export file — branch on
//! [`CoordDb::backend`] or refuse with [`DbError::SqliteOnly`].
//!
//! **THE COORDINATOR NEVER OPENS A v2 DATABASE.** It has its own data
//! directory, its own database name, and its own squashed migration, and there
//! is no code path by which it could read a file the previous product wrote.
//! The one exception is deliberate and lives in another crate: `roost
//! import-v2` ATTACHes a v2 database read-only, once, before this install's
//! first boot, to carry paired browsers across a cutover. It is an operator
//! command, not a boot path, and a coordinator that has started has nothing
//! left to import — which is why `roost import-v2` refuses to run while one is
//! running.
//!
//! ONE CONNECTION, NOT A POOL, ON EITHER BACKEND. v2 opens a single
//! `bun:sqlite` handle and Kysely reads and writes through it
//! (`apps/coord/src/db/connection.ts:26-49`); concurrency comes from WAL plus a
//! busy timeout plus the in-process write gate. Modelling a reader/writer pool
//! would be a behavioural change, not an optimisation — and on Postgres it is
//! a correctness change: see [`POSTGRES_POOL_SIZE`].
//!
//! `synchronous = NORMAL` IS DELIBERATE AND MUST NOT BE "FIXED" TO FULL. The
//! comment that settles it (`connection.ts:26-33`): "WAL + synchronous=NORMAL is
//! SQLite's documented pairing -- commits stop fsyncing (only checkpoints do).
//! synchronous=FULL (the default this replaces) put a WAL fsync on the
//! event-loop thread inside writeAuditLog, which runs for every SessionsInput
//! RPC, i.e. once per keystroke batch; that fsync starves the cell fan-out
//! exactly while the user is typing." The cost is the last transaction or two
//! on an OS crash or power loss, not a process crash, and the `events` table is
//! re-derivable from the worker snapshot every worker emits on reconnect.
//!
//! The schema is `migrations/sqlite/0001_init.sql` and its lockstep Postgres
//! translation `migrations/postgres/0001_init.sql`; every domain owns its row
//! type. `db/migration_validation.rs` ports `migration-validation.ts`.

mod backend;
mod in_list;
mod migration_validation;
pub mod postgres_to_sqlite;
mod sql_builder;
pub mod sqlite_to_postgres;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use roost_host::DatabaseLocation;

pub use backend::{POSTGRES_POOL_SIZE, sqlite_url};
pub use in_list::{IN_LIST_CHUNK, push_in_list};
pub use sql_builder::{Separated, SqlBuilder};

/// Which server is behind a [`CoordDb`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbBackend {
    /// A SQLite file this coordinator owns.
    Sqlite,
    /// An external Postgres server.
    Postgres,
}

impl DbBackend {
    /// The backend's name in logs.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
        }
    }
}

/// The coordinator's database handle: the pool, its backend, and the SQLite
/// file it owns when it owns one.
#[derive(Debug, Clone)]
pub struct CoordDb {
    pool: sqlx::AnyPool,
    backend: DbBackend,
    sqlite_path: Option<PathBuf>,
}

/// Why the database could not be opened or migrated.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The pool could not be created, or a statement failed.
    #[error("database: {0}")]
    Sqlite(#[from] sqlx::Error),
    /// A migration could not be read out of the embedded set.
    #[error("migration {name}: {reason}")]
    Migration { name: String, reason: String },
    /// A migration ran and left the file inconsistent, which is the only way a
    /// partial migration can surface and is never recoverable in place.
    #[error("migration {name} failed: {reason}")]
    MigrationFailed { name: String, reason: String },
    /// The pre-migration backup could not be taken, so the migration did not
    /// run. Its own variant because it is NOT a migration failure: the
    /// migration is fine, the thing that protects it is not.
    #[error("pre-migration backup failed, so no migration ran: {0}")]
    PreMigrationBackup(String),
    /// SQLite did not keep foreign-key enforcement on this connection.
    #[error("SQLite foreign key enforcement is required for migrations")]
    ForeignKeysUnenforced,
    /// Rows violate a foreign key; `violations` names each `table -> parent`.
    #[error("foreign key check failed ({rows} rows): {violations}")]
    ForeignKeyCheck { rows: usize, violations: String },
    /// The applied history names a migration this build does not embed and does
    /// not declare retired.
    ///
    /// Its own variant, and not [`Self::Migration`], because the two answer
    /// different questions. A failure to RUN a migration is a broken binary; an
    /// unrecognised history row is a file this coordinator did not write, and
    /// the operator's next question is "which build made this", which the
    /// version number answers and `name: "embedded"` never could.
    #[error(
        "migration history names version {version}, which this build neither \
         embeds nor declares retired; the file was written by another build, so \
         refuse it rather than guess -- a v2 database lives in a different data \
         directory and `roost import-v2` is how an identity crosses over"
    )]
    UnknownMigration { version: i64 },
    /// A file-level operation was asked of a database that is not a file.
    #[error("{0} needs the SQLite backend")]
    SqliteOnly(&'static str),
}

/// How long a statement waits for a write lock, or a caller for a pooled
/// connection, before giving up.
///
/// 5 s (`connection.ts:30`): "prevents SQLITE_BUSY under light write
/// contention". Above the write gate's own hold time, so a mutation queued behind
/// an exclusive keeper-update drain waits rather than failing.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Migrations that shipped and were then removed, by version.
///
/// A removed migration's row is TRUE — that database did apply it — so reading
/// it as a corrupt history bricks a coordinator that was working minutes ago,
/// which is the incident `docs/FAILURE-INDEX.md` records. v2 solved it by
/// listing the retired NAME; `sqlx` keys its history by VERSION, so the
/// retirement is a version here and the comparison is on the version rather
/// than on a position in the raw history. A reused slot number therefore
/// cannot make a survivor look out of order, because no survivor is ever
/// compared by position.
///
/// EMPTY, and it stays empty for as long as the embedded set is the one squashed
/// migration: `sqlx` checksums every applied file, so `0001_init.sql` is frozen
/// the moment it ships anywhere and a migration cannot be edited out from under
/// a live install. The list exists because a retirement has to be DECLARED and
/// never inferred from a row's absence — an undeclared one keeps failing closed,
/// which is the half of this that protects a database nothing has heard of.
pub const RETIRED_MIGRATIONS: &[i64] = &[];

/// Whether this build's history decision admits `applied`.
///
/// Two rules at one gate, so there is nowhere for a second opinion to hide: an
/// applied version has to be embedded or declared retired, and a retirement
/// must name a migration this build does NOT embed — declaring one that is still
/// here would turn the checksum guard on a shipped file into a no-op and hide
/// the mistake behind a name that reads as deliberate.
///
/// Comparison is by version, never by position in the raw history: a retired
/// version sorts wherever it sorts, and v2's reused slot 0017 is the shape that
/// made an ordinal comparison diverge at the position it read from.
pub fn validate_migration_history<'a>(
    applied: impl IntoIterator<Item = &'a i64>,
    embedded: &HashSet<i64>,
    retired: &[i64],
) -> Result<(), DbError> {
    for version in retired {
        if embedded.contains(version) {
            return Err(DbError::UnknownMigration { version: *version });
        }
    }
    for version in applied {
        if embedded.contains(version) || retired.contains(version) {
            continue;
        }
        return Err(DbError::UnknownMigration { version: *version });
    }
    Ok(())
}

/// Open the coordinator's database, creating and migrating it if needed.
///
/// Migrations run before this returns, so nothing above this line ever observes
/// a schema that is one migration behind -- and on SQLite neither does anything
/// observe a file with foreign-key enforcement off or a row violating one: both
/// refuse the open, which is what keeps a damaged database from ever binding a
/// port. Postgres enforces foreign keys unconditionally and owns its own
/// integrity, so those checks are SQLite's alone.
pub async fn open(location: &DatabaseLocation) -> Result<CoordDb, DbError> {
    sqlx::any::install_default_drivers();
    // Captured before the connect, which creates the file: this is the gate
    // the pre-migration backup hangs on. A Postgres database always "existed":
    // its backups are the server operator's.
    let (database, existed) = match location {
        DatabaseLocation::SqliteFile(path) => {
            let existed = path.exists();
            let pool = backend::connect_sqlite(path).await?;
            let database = CoordDb {
                pool,
                backend: DbBackend::Sqlite,
                sqlite_path: Some(path.clone()),
            };
            (database, existed)
        }
        DatabaseLocation::Postgres(url) => {
            let pool = backend::connect_postgres(url).await?;
            let database = CoordDb {
                pool,
                backend: DbBackend::Postgres,
                sqlite_path: None,
            };
            (database, true)
        }
    };
    let backend = database.backend.label();

    // v2 verifies enforcement before it reads its migration history
    // (`migrate.ts:292-293`); a migration never runs on an unenforced handle.
    migration_validation::enable_and_verify_foreign_keys(&database).await?;
    let mut migrator = match database.backend {
        DbBackend::Sqlite => sqlx::migrate!("./migrations/sqlite"),
        DbBackend::Postgres => sqlx::migrate!("./migrations/postgres"),
    };
    let applied = applied_migrations(&database).await?;
    // Before the backup and before anything runs: a history this build does not
    // recognise is a file it must not write to, and spending one of the
    // fourteen kept archives to discover that would be the wrong order.
    validate_migration_history(
        &applied,
        &migrator.iter().map(|m| m.version).collect(),
        RETIRED_MIGRATIONS,
    )?;
    migrator.set_ignore_missing(true);
    let pending = pending_migrations(&migrator, &applied);
    if !pending.is_empty() {
        tracing::info!(backend, pending = ?pending, "database migrations pending");
    }

    // Boot step 3 (contract §1.1): the pre-migration backup, ONLY if the file
    // already existed (`main.ts:60`, backing up a file the connect just created
    // is theatre) AND a migration is pending (`migrate.ts:319`, a restart that
    // migrates nothing must not spend one of the fourteen kept archives).
    //
    // A failure here STOPS the open. v2 passes this as a hook into
    // `runMigrations` (`main.ts:64`), so a backup that cannot be taken fails
    // the migration with it -- if there is no recoverable copy, a destructive
    // migration must not proceed.
    if existed && !pending.is_empty() && database.backend == DbBackend::Sqlite {
        crate::maintenance::backup::run_backup(
            &database,
            crate::maintenance::backup::BackupReason::PreMigration,
        )
        .await
        .map_err(|error| DbError::PreMigrationBackup(error.to_string()))?;
    }
    database.apply_migrations(&migrator).await?;
    migration_validation::enable_and_verify_foreign_keys(&database).await?;
    if let Some(final_migration) = pending.last() {
        migration_validation::validate_integrity(&database, final_migration).await?;
        tracing::info!(backend, applied = pending.len(), "database migrated");
    }
    // Every open, not only after a migration as v2 gates it (`migrate.ts:343`):
    // with one squashed migration that gate would only ever check a file that
    // was empty a moment ago, while `roost import-v2` writes rows into an
    // already-migrated one.
    migration_validation::validate_foreign_keys(&database).await?;
    Ok(database)
}

/// Every version `_sqlx_migrations` records, or nothing on a database that has
/// never been migrated.
///
/// Read from `_sqlx_migrations` directly rather than through `Migrate`, whose
/// listing creates that table first -- a write the pre-migration backup must
/// not be taken after.
async fn applied_migrations(database: &CoordDb) -> Result<HashSet<i64>, DbError> {
    if !backend::migration_history_exists(database).await? {
        return Ok(HashSet::new());
    }
    Ok(
        sqlx::query_as::<_, (i64,)>("SELECT version FROM _sqlx_migrations")
            .fetch_all(database.pool())
            .await?
            .into_iter()
            .map(|(version,)| version)
            .collect(),
    )
}

/// The embedded migrations this database has not applied yet, by description.
fn pending_migrations(migrator: &sqlx::migrate::Migrator, applied: &HashSet<i64>) -> Vec<String> {
    migrator
        .iter()
        .filter(|migration| {
            !migration.migration_type.is_down_migration() && !applied.contains(&migration.version)
        })
        .map(|migration| format!("{:04}_{}", migration.version, migration.description))
        .collect()
}

impl CoordDb {
    /// The pool: one connection on either backend (see [`POSTGRES_POOL_SIZE`]).
    #[must_use]
    pub fn pool(&self) -> &sqlx::AnyPool {
        &self.pool
    }

    /// Which server is behind this handle.
    #[must_use]
    pub fn backend(&self) -> DbBackend {
        self.backend
    }

    /// The SQLite file this handle owns, or `None` on Postgres — the switch
    /// every file-level feature (backup, export, integrity check) turns on.
    #[must_use]
    pub fn sqlite_path(&self) -> Option<&Path> {
        self.sqlite_path.as_deref()
    }

    /// Apply every pending embedded migration.
    ///
    /// `sqlx` records applied migrations in `_sqlx_migrations` and wraps each in
    /// a transaction, so a partial migration is not a state this function can
    /// leave behind. It also stores a **checksum**, which v2's bespoke runner did
    /// not (`apps/coord/src/db/migrate.ts:295-300` stored only a name and a
    /// timestamp) -- a stricter contract, and the reason `0001_init.sql` must
    /// never be edited after it has shipped anywhere. Editing it would produce a
    /// v3 install whose database does not match its migrations, with something
    /// to notice.
    async fn apply_migrations(&self, migrator: &sqlx::migrate::Migrator) -> Result<(), DbError> {
        migrator
            .run(&self.pool)
            .await
            .map_err(|error| DbError::Migration {
                name: "embedded".to_string(),
                reason: error.to_string(),
            })
    }

    /// Run `PRAGMA integrity_check` and require exactly `ok`.
    ///
    /// Used before an export snapshot is published, so a corrupt file is never
    /// handed to an operator as a backup
    /// (`apps/coord/src/db/snapshot.ts:24-26`).
    pub async fn integrity_check(&self) -> Result<bool, DbError> {
        self.require_sqlite("integrity_check")?;
        let row: (String,) = sqlx::query_as("PRAGMA integrity_check")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0 == "ok")
    }

    /// Take a consistent, compacted copy of the database into `destination`.
    ///
    /// `VACUUM INTO` produces a standalone, transactionally consistent copy
    /// without blocking readers (`apps/coord/src/db/snapshot.ts:19`). The caller
    /// is responsible for the mode, the integrity check and removing a partial
    /// file on failure -- all three are steps, not options, and a snapshot that
    /// skipped any of them is the failure this ordering exists to prevent.
    pub async fn vacuum_into(&self, destination: &Path) -> Result<(), DbError> {
        self.require_sqlite("vacuum_into")?;
        // The path is BOUND, never interpolated. SQLite's `VACUUM INTO` takes an
        // expression, so a bound parameter is both correct and the only spelling
        // that cannot turn a database path into SQL.
        sqlx::query("VACUUM INTO $1")
            .bind(destination.to_string_lossy().into_owned())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    fn require_sqlite(&self, operation: &'static str) -> Result<(), DbError> {
        match self.backend {
            DbBackend::Sqlite => Ok(()),
            DbBackend::Postgres => Err(DbError::SqliteOnly(operation)),
        }
    }
}
