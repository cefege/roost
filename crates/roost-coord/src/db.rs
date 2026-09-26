//! Opening the coordinator's SQLite file, and the pragmas that make one
//! connection enough.
//!
//! Owned by the coordinator. Everything above this file takes a `CoordDb` and
//! never opens a connection, so the pragma set and the migration are stated in
//! exactly one place.
//!
//! ONE CONNECTION, NOT A POOL. v2 opens a single `bun:sqlite` handle and Kysely
//! reads and writes through it (`apps/coord/src/db/connection.ts:26-49`);
//! concurrency comes from WAL plus a busy timeout plus the in-process write gate.
//! Modelling a reader/writer pool would be a behavioural change, not an
//! optimisation, so this type is a pool of one and says so.
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

use std::path::Path;
use std::time::Duration;

/// The coordinator's database handle: a pool of one, plus the path it owns.
#[derive(Debug, Clone)]
pub struct CoordDb {
    pool: sqlx::SqlitePool,
    path: std::path::PathBuf,
}

/// Why the database could not be opened or migrated.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The pool could not be created, or a statement failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    /// A migration could not be read out of the embedded set.
    #[error("migration {name}: {reason}")]
    Migration { name: String, reason: String },
    /// A migration ran and left the file inconsistent, which is the only way a
    /// partial migration can surface and is never recoverable in place.
    #[error("migration {name} failed: {reason}")]
    MigrationFailed { name: String, reason: String },
}

/// How long a statement waits for a write lock before giving up.
///
/// 5 s (`connection.ts:30`): "prevents SQLITE_BUSY under light write
/// contention". Above the write gate's own hold time, so a mutation queued behind
/// an exclusive keeper-update drain waits rather than failing.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Open the coordinator's database, creating and migrating it if needed.
///
/// Migrations run before this returns, so nothing above this line ever observes
/// a schema that is one migration behind.
pub async fn open(path: &Path) -> Result<CoordDb, DbError> {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        // See the module header: this is the load-bearing pair, and the
        // performance note is the incident that fixed it.
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        .busy_timeout(BUSY_TIMEOUT)
        .foreign_keys(true)
        // Negative means KiB, not pages: 8 MiB explicitly rather than the
        // implicit 2 MiB default (`connection.ts:36`).
        .pragma("cache_size", "-8000")
        // Mapped pages count against the cgroup; keep them out of RSS
        // (`connection.ts:38`).
        .pragma("mmap_size", "0")
        // A 64 MiB backstop on one query's allocations (`connection.ts:40`).
        .pragma("soft_heap_limit", "67108864")
        // A ~4 MiB WAL target, pinned so a config change is visible
        // (`connection.ts:42`).
        .pragma("wal_autocheckpoint", "1000")
        // Truncate the -wal back to 32 MiB after a burst instead of never
        // (`connection.ts:44`).
        .pragma("journal_size_limit", "33554432");

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        // One connection, for the reason in the module header.
        .max_connections(1)
        .acquire_timeout(BUSY_TIMEOUT)
        .connect_with(options)
        .await?;

    let database = CoordDb {
        pool,
        path: path.to_path_buf(),
    };
    database.migrate().await?;
    Ok(database)
}

impl CoordDb {
    /// The pool. A pool of one, so a caller that sees a pool type is not tempted
    /// to scale it.
    #[must_use]
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// The file this handle owns.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Apply every embedded migration.
    ///
    /// `sqlx` records applied migrations in `_sqlx_migrations` and wraps each in
    /// a transaction, so a partial migration is not a state this function can
    /// leave behind. It also stores a **checksum**, which v2's bespoke runner did
    /// not (`apps/coord/src/db/migrate.ts:295-300` stored only a name and a
    /// timestamp) -- a stricter contract, and the reason `0001_init.sql` must
    /// never be edited after it has shipped anywhere. Editing it would produce a
    /// v3 install whose database does not match its migrations, with something
    /// to notice.
    pub async fn migrate(&self) -> Result<(), DbError> {
        sqlx::migrate!("./migrations")
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
        let row: (String,) = sqlx::query_as("PRAGMA integrity_check")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0 == "ok")
    }

    /// Run `PRAGMA foreign_key_check` and require no rows.
    ///
    /// The same gate the v2 migration runner applies after the final pending
    /// migration (`apps/coord/src/db/migration-validation.ts:20-35`).
    pub async fn foreign_key_check(&self) -> Result<bool, DbError> {
        let rows = sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.is_empty())
    }

    /// Take a consistent, compacted copy of the database into `destination`.
    ///
    /// `VACUUM INTO` produces a standalone, transactionally consistent copy
    /// without blocking readers (`apps/coord/src/db/snapshot.ts:19`). The caller
    /// is responsible for the mode, the integrity check and removing a partial
    /// file on failure -- all three are steps, not options, and a snapshot that
    /// skipped any of them is the failure this ordering exists to prevent.
    pub async fn vacuum_into(&self, destination: &Path) -> Result<(), DbError> {
        // The path is BOUND, never interpolated. SQLite's `VACUUM INTO` takes an
        // expression, so a bound parameter is both correct and the only spelling
        // that cannot turn a database path into SQL.
        sqlx::query("VACUUM INTO ?")
            .bind(destination.to_string_lossy())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
