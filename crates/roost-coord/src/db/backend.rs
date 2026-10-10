//! Connecting to each backend, and the per-backend probes `db::open` needs
//! before a migration runs.
//!
//! Owned by `crate::db`; `db::open` is the only caller. The SQLite pragma set
//! lives here because `AnyConnectOptions` is URL-only and a SQLite URL carries
//! none of them, so each new connection applies them in `after_connect`.

use std::path::Path;
use std::time::Duration;

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

/// How long a booting coordinator keeps retrying a Postgres it cannot reach.
///
/// A new pod's IP is admitted by the Postgres NetworkPolicy asynchronously, so
/// the first connect after a rollout can fail for a few seconds.
pub const POSTGRES_CONNECT_BUDGET: Duration = Duration::from_secs(60);

/// The wait before connect attempt `attempt + 1`: 500 ms doubling, capped at 5 s.
#[must_use]
pub fn postgres_connect_delay(attempt: u32) -> Duration {
    const FIRST_DELAY_MS: u64 = 500;
    const MAX_DELAY_MS: u64 = 5_000;
    let factor = 1_u64.checked_shl(attempt).unwrap_or(u64::MAX);
    Duration::from_millis(FIRST_DELAY_MS.saturating_mul(factor).min(MAX_DELAY_MS))
}

/// Whether a failed connect can succeed on a later attempt.
#[must_use]
pub fn postgres_connect_is_transient(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::PoolTimedOut => true,
        sqlx::Error::Database(database_error) => matches!(
            database_error.code().as_deref(),
            Some("57P03" | "53300" | "08000" | "08001" | "08006")
        ),
        _ => false,
    }
}

/// A pool over the Postgres server at `url`, retrying a transient failure
/// within [`POSTGRES_CONNECT_BUDGET`].
pub(super) async fn connect_postgres(url: &str) -> Result<AnyPool, DbError> {
    let started = tokio::time::Instant::now();
    let mut attempt: u32 = 0;
    loop {
        let connected = AnyPoolOptions::new()
            .max_connections(POSTGRES_POOL_SIZE)
            .acquire_timeout(BUSY_TIMEOUT)
            .connect(url)
            .await;
        match connected {
            Ok(pool) => {
                if attempt > 0 {
                    tracing::info!(attempts = attempt + 1, "postgres reachable after retrying");
                }
                return Ok(pool);
            }
            Err(error) => {
                let delay = postgres_connect_delay(attempt);
                if !postgres_connect_is_transient(&error)
                    || started.elapsed() + delay >= POSTGRES_CONNECT_BUDGET
                {
                    return Err(error.into());
                }
                tracing::warn!(
                    attempt = attempt + 1,
                    delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    error = %error,
                    "postgres unreachable at boot; retrying"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{postgres_connect_delay, postgres_connect_is_transient};

    #[test]
    fn the_connect_delay_doubles_from_half_a_second_to_a_five_second_cap() {
        let delays: Vec<Duration> = [0, 1, 2, 3, 4, 10]
            .into_iter()
            .map(postgres_connect_delay)
            .collect();
        let expected: Vec<Duration> = [500, 1_000, 2_000, 4_000, 5_000, 5_000]
            .into_iter()
            .map(Duration::from_millis)
            .collect();
        assert_eq!(delays, expected);
        assert_eq!(postgres_connect_delay(u32::MAX), Duration::from_secs(5));
    }

    #[test]
    fn an_unreachable_server_is_retried_and_a_misconfiguration_is_not() {
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        assert!(postgres_connect_is_transient(&sqlx::Error::Io(refused)));
        assert!(postgres_connect_is_transient(&sqlx::Error::PoolTimedOut));
        assert!(!postgres_connect_is_transient(&sqlx::Error::Configuration(
            "bad url".into()
        )));
        assert!(!postgres_connect_is_transient(&sqlx::Error::RowNotFound));
    }
}
