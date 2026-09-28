//! The durable outbox's file-level guarantees: the two pragmas that make an
//! un-acknowledged row survive a crash, the schema this build owns, and the
//! page budget that stops the file growing past its cap. Called by
//! [`super::Journal::open`] and by nothing else.
//!
//! Every check here is a REFUSAL rather than a repair. The outbox is the only
//! record of what this worker did while the coordinator was unreachable, so a
//! file this build cannot prove it owns is not migrated, not emptied and not
//! quietly adopted: a worker that starts anyway would accept events it cannot
//! replay, and the loss would surface as a session the coordinator never heard
//! of rather than as a store that would not start.
//! Ports v2 `apps/worker/src/transport/session-event-store-schema.ts`.

use sqlx::AssertSqlSafe;
use sqlx::sqlite::SqlitePool;

use super::super::MAX_DATABASE_BYTES;
use super::{JournalError, corrupt, query};

/// The schema this build owns.
///
/// A file at any other version is REFUSED, not migrated, and version 1 — the
/// rows-and-sequences-only schema — is refused exactly as a foreign one is. The
/// outbox became reachable from the composition root hours ago and holds no
/// production data, so there is nothing to migrate FROM; a worker that met one
/// stops and says so instead of quietly starting a second, empty store beside
/// the first, which is the one outcome nobody would notice.
pub const SCHEMA_VERSION: i64 = 2;

/// The version stamp, as a literal so the query is a `&'static str` and needs no
/// injection assertion for a number that cannot change.
const USER_VERSION_PRAGMA: &str = "PRAGMA user_version = 2";

const _: () = assert!(
    SCHEMA_VERSION == 2,
    "USER_VERSION_PRAGMA must name the version above"
);

/// The schema, in the order the statements depend on each other.
///
/// CHECK constraints rather than a STRICT table: the guards are the same and do
/// not depend on how the bundled SQLite happened to be compiled.
const SCHEMA: [&str; 4] = [
    "CREATE TABLE sequence_state (
        singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
        reserved_through INTEGER NOT NULL CHECK(reserved_through >= 0)
    )",
    "CREATE TABLE session_events (
        client_seq INTEGER PRIMARY KEY,
        kind TEXT NOT NULL CHECK(length(kind) > 0),
        event_json TEXT NOT NULL CHECK(length(event_json) > 0),
        payload_bytes INTEGER NOT NULL CHECK(payload_bytes > 0)
    )",
    // A claim is capacity a session has taken for an event that does not exist
    // yet. It is PERSISTED because the case it exists for is a machine that
    // reboots mid-spawn, and LEASED because persistence makes the other failure
    // permanent: a claim from a process that died would hold capacity for ever
    // and the store would eventually refuse every write while reporting "full".
    "CREATE TABLE session_claims (
        id INTEGER PRIMARY KEY,
        kind TEXT NOT NULL CHECK(length(kind) > 0),
        payload_bytes INTEGER NOT NULL CHECK(payload_bytes > 0),
        claimed_at_ms INTEGER NOT NULL,
        snapshot_blocking INTEGER NOT NULL CHECK(snapshot_blocking IN (0, 1))
    )",
    "INSERT INTO sequence_state (singleton, reserved_through) VALUES (1, 0)",
];

/// Prove the file is one this build can own, and cap it.
pub async fn establish(pool: &SqlitePool) -> Result<(), JournalError> {
    verify_pragmas(pool).await?;
    check_integrity(pool).await?;
    ensure_tables(pool).await?;
    enforce_page_budget(pool).await
}

/// The highest sequence already on disk.
///
/// A restart has to resume above this: handing out a number a previous process
/// already burned would let a replayed event be mistaken for a new one, and
/// that is the one sequence defect a durable store cannot recover from.
pub async fn persisted_high_water(pool: &SqlitePool) -> Result<u64, JournalError> {
    let (persisted, reserved): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(MAX(client_seq), 0), (SELECT reserved_through FROM sequence_state \
         WHERE singleton = 1) FROM session_events",
    )
    .fetch_one(pool)
    .await
    .map_err(query("high water read"))?;
    if persisted < 0 || reserved < 0 {
        return Err(corrupt("a client sequence is negative"));
    }
    if persisted > reserved {
        return Err(corrupt(
            "a row carries a sequence past the reserved watermark, so a number this file never \
             burned has already been used",
        ));
    }
    u64::try_from(persisted).map_err(|_| corrupt("a client sequence is not storable"))
}

async fn verify_pragmas(pool: &SqlitePool) -> Result<(), JournalError> {
    let (journal,): (String,) = sqlx::query_as("PRAGMA journal_mode")
        .fetch_one(pool)
        .await
        .map_err(query("journal mode read"))?;
    if !journal.eq_ignore_ascii_case("delete") {
        return Err(JournalError::Configuration {
            reason: format!(
                "journal mode is {journal:?}, and a write-ahead log would hold an un-acknowledged \
                 row outside the file this worker has to recover"
            ),
        });
    }
    let (synchronous,): (i64,) = sqlx::query_as("PRAGMA synchronous")
        .fetch_one(pool)
        .await
        .map_err(query("synchronous read"))?;
    // The integer, not the enum: `PRAGMA synchronous` reports 0/1/2 and there is
    // no string form to compare, so the constant is the one SQLite defines.
    if synchronous != 2 {
        return Err(JournalError::Configuration {
            reason: format!(
                "synchronous is {synchronous}, and FULL (2) is the only level that makes an \
                 acknowledged event durable"
            ),
        });
    }
    Ok(())
}

async fn check_integrity(pool: &SqlitePool) -> Result<(), JournalError> {
    let (verdict,): (String,) = sqlx::query_as("PRAGMA integrity_check")
        .fetch_one(pool)
        .await
        .map_err(query("integrity check"))?;
    if verdict != "ok" {
        return Err(JournalError::Configuration {
            reason: format!("integrity check reported {verdict:?}"),
        });
    }
    Ok(())
}

/// Create the schema, or prove the file already holds the one this build wrote.
async fn ensure_tables(pool: &SqlitePool) -> Result<(), JournalError> {
    let tables: Vec<String> = sqlx::query_as::<_, (String,)>(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
             ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .map_err(query("schema read"))?
    .into_iter()
    .map(|(name,)| name)
    .collect();
    if tables.is_empty() {
        return create_tables(pool).await;
    }
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(pool)
        .await
        .map_err(query("schema version read"))?;
    if version != SCHEMA_VERSION || tables != ["sequence_state", "session_claims", "session_events"]
    {
        return Err(JournalError::Schema {
            reason: format!("user_version {version} with tables {tables:?}"),
        });
    }
    Ok(())
}

async fn create_tables(pool: &SqlitePool) -> Result<(), JournalError> {
    let mut transaction = pool.begin().await.map_err(query("begin"))?;
    for statement in SCHEMA {
        sqlx::query(statement)
            .execute(&mut *transaction)
            .await
            .map_err(query("schema creation"))?;
    }
    sqlx::query(USER_VERSION_PRAGMA)
        .execute(&mut *transaction)
        .await
        .map_err(query("schema version write"))?;
    transaction.commit().await.map_err(query("commit"))
}

/// Cap the file at [`MAX_DATABASE_BYTES`], page overhead included.
///
/// `max_page_count` is persistent, so the cap outlives this process: a store
/// that was allowed past it once must not be allowed to stay there.
async fn enforce_page_budget(pool: &SqlitePool) -> Result<(), JournalError> {
    let (page_size,): (i64,) = sqlx::query_as("PRAGMA page_size")
        .fetch_one(pool)
        .await
        .map_err(query("page size read"))?;
    if page_size <= 0 {
        return Err(corrupt("the page size is not a positive number of bytes"));
    }
    let page_size =
        u64::try_from(page_size).map_err(|_| corrupt("the page size is not a count"))?;
    let max_pages = (MAX_DATABASE_BYTES / page_size).max(1);
    // `max_pages` is a `u64` this function just derived from a page size, so
    // there is nothing in it to escape; sqlx cannot see that through a `format!`.
    sqlx::query(AssertSqlSafe(format!(
        "PRAGMA max_page_count = {max_pages}"
    )))
    .execute(pool)
    .await
    .map_err(query("page budget write"))?;
    let (pages,): (i64,) = sqlx::query_as("PRAGMA page_count")
        .fetch_one(pool)
        .await
        .map_err(query("page count read"))?;
    if i64::try_from(max_pages).unwrap_or(i64::MAX) < pages {
        return Err(JournalError::Configuration {
            reason: format!("the file already holds {pages} pages against a budget of {max_pages}"),
        });
    }
    Ok(())
}
