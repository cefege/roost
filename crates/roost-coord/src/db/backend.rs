//! Connecting to each backend, and the per-backend probes `db::open` needs
//! before a migration runs.
//!
//! Owned by `crate::db`; `db::open` is the only caller. The SQLite pragma set
//! lives here because `AnyConnectOptions` is URL-only and a SQLite URL carries
//! none of them, so each new connection applies them in `after_connect`.

use std::path::Path;

use sqlx::AnyPool;
use sqlx::any::AnyPoolOptions;

use super::{BUSY_TIMEOUT, CoordDb, DbBackend, DbError};

/// Connections a Postgres pool may open: ONE, as on SQLite.
///
/// Every count-then-write in this crate — the push subscription cap, the
/// pending pair-request cap, single-use grant redemption — is one statement
/// that is correct only when no other write interleaves with it. SQLite's
/// single connection gives that for free; under Postgres's READ COMMITTED a
/// second connection lets two such statements both see room and both land.
/// One connection restores the ordering for every statement at once, where
/// per-statement locks would have to be found and kept, one call site at a time.
pub const POSTGRES_POOL_SIZE: u32 = 1;

/// The Postgres advisory-lock key [`CoordDb::begin_write`] serializes on: the
/// coordinator port, so it reads as this product's in `pg_locks`.
const WRITE_LOCK_KEY: i64 = 4113;

/// Applied, in order, to every SQLite connection the pool opens.
const SQLITE_PRAGMAS: &[&str] = &[
    "PRAGMA journal_mode = WAL",
    // See the `db` module header: this is the load-bearing pair, and the
    // performance note is the incident that fixed it.
    "PRAGMA synchronous = NORMAL",
    // `BUSY_TIMEOUT`, in milliseconds.
    "PRAGMA busy_timeout = 5000",
    "PRAGMA foreign_keys = ON",
    // Negative means KiB, not pages: 8 MiB explicitly rather than the implicit
    // 2 MiB default (`connection.ts:36`).
    "PRAGMA cache_size = -8000",
    // Mapped pages count against the cgroup; keep them out of RSS
    // (`connection.ts:38`).
    "PRAGMA mmap_size = 0",
    // A 64 MiB backstop on one query's allocations (`connection.ts:40`).
    "PRAGMA soft_heap_limit = 67108864",
    // A ~4 MiB WAL target, pinned so a config change is visible
    // (`connection.ts:42`).
    "PRAGMA wal_autocheckpoint = 1000",
    // Truncate the -wal back to 32 MiB after a burst instead of never
    // (`connection.ts:44`).
    "PRAGMA journal_size_limit = 33554432",
];

/// A pool of one over the SQLite file at `path`, created if missing.
pub(super) async fn connect_sqlite(path: &Path) -> Result<AnyPool, DbError> {
    let url = sqlite_url(path);
    Ok(AnyPoolOptions::new()
        // One connection, for the reason in the `db` module header.
        .max_connections(1)
        .acquire_timeout(BUSY_TIMEOUT)
        .after_connect(|connection, _metadata| {
            Box::pin(async move {
                for pragma in SQLITE_PRAGMAS {
                    sqlx::query(*pragma).execute(&mut *connection).await?;
                }
                Ok(())
            })
        })
        .connect(url.as_str())
        .await?)
}

/// The `sqlite:` URL the `Any` driver opens `path` from, created if missing.
/// Public so a test's second connection opens the file the coordinator does.
#[cfg(unix)]
pub fn sqlite_url(path: &Path) -> String {
    use sqlx::ConnectOptions as _;
    sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .to_url_lossy()
        .to_string()
}

/// The `sqlite:` URL the `Any` driver opens `path` from, created if missing.
///
/// Written as an opaque `sqlite:C:/…` URL rather than through `to_url_lossy`,
/// whose `sqlite://C:\…` form reads the drive as a host and reopens a
/// different path. SQLite accepts `/` as a Windows separator; `%`, `?` and `#`
/// are escaped because the driver percent-decodes the path and splits off the
/// query at `?`.
#[cfg(windows)]
pub fn sqlite_url(path: &Path) -> String {
    let path = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace('%', "%25")
        .replace('?', "%3F")
        .replace('#', "%23");
    format!("sqlite:{path}?mode=rwc")
}

/// A pool over the Postgres server at `url`.
pub(super) async fn connect_postgres(url: &str) -> Result<AnyPool, DbError> {
    Ok(AnyPoolOptions::new()
        .max_connections(POSTGRES_POOL_SIZE)
        .acquire_timeout(BUSY_TIMEOUT)
        .connect(url)
        .await?)
}

/// Whether `_sqlx_migrations` exists, asked through each backend's catalog so
/// the answer never creates it.
pub(super) async fn migration_history_exists(database: &CoordDb) -> Result<bool, DbError> {
    let probe = match database.backend() {
        DbBackend::Sqlite => {
            "SELECT COUNT(*) FROM sqlite_master \
              WHERE type = 'table' AND name = '_sqlx_migrations'"
        }
        DbBackend::Postgres => {
            "SELECT COUNT(*) FROM information_schema.tables \
              WHERE table_schema = current_schema() AND table_name = '_sqlx_migrations'"
        }
    };
    let (tables,): (i64,) = sqlx::query_as(probe).fetch_one(database.pool()).await?;
    Ok(tables > 0)
}

impl CoordDb {
    /// A transaction that holds the database's write lock from its first
    /// statement, so a read-then-insert inside it is one decision.
    ///
    /// SQLite spells it `BEGIN IMMEDIATE`. Postgres has no database-wide write
    /// lock, so the transaction takes one transaction-scoped advisory lock that
    /// every `begin_write` caller shares: a second caller blocks until the first
    /// commits, then reads what it wrote — the same serialization, released by
    /// commit or rollback exactly as SQLite's is.
    pub async fn begin_write(&self) -> Result<sqlx::Transaction<'_, sqlx::Any>, sqlx::Error> {
        match self.backend() {
            DbBackend::Sqlite => self.pool().begin_with("BEGIN IMMEDIATE").await,
            DbBackend::Postgres => {
                let mut transaction = self.pool().begin().await?;
                sqlx::query("SELECT pg_advisory_xact_lock($1)")
                    .bind(WRITE_LOCK_KEY)
                    .execute(&mut *transaction)
                    .await?;
                Ok(transaction)
            }
        }
    }
}
