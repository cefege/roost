//! `db::sqlite_to_postgres` against a real Postgres server: every row and byte
//! lands, identity sequences continue past the copied ids, a non-empty target
//! is refused unless replaced, and a replace does not double the rows.
//!
//! The copy needs a Postgres target, so the tests that perform one run only
//! when `ROOST_TEST_DATABASE_URL` is set — CI's Postgres job sets it.

use std::path::PathBuf;

use roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant;
use roost_coord::db::sqlite_to_postgres::{ExistingRows, TransferError, copy_sqlite_to_postgres};
use roost_coord::db::{self, CoordDb};
use roost_host::DatabaseLocation;

use super::db_support;

const KEY_BYTES: [u8; 32] = [0xab; 32];

/// A scratch directory holding the source file and the target's marker.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("roost-sqlite-to-pg-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Self { root }
    }

    fn source(&self) -> PathBuf {
        self.root.join("source.db")
    }

    /// A fresh Postgres database, or `None` when the suite runs on SQLite only.
    async fn target(&self) -> Option<String> {
        if !db_support::running_on_postgres() {
            return None;
        }
        match db_support::test_database_location(&self.root.join("target")).await {
            DatabaseLocation::Postgres(url) => Some(url),
            DatabaseLocation::SqliteFile(_) => None,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A source with a tenant, a key plus its revocation (an order the target's
/// insert triggers would refuse), and events whose ids have a gap.
async fn seed_source(path: &std::path::Path) {
    let source = db::open(&DatabaseLocation::SqliteFile(path.to_path_buf()))
        .await
        .expect("a migrated SQLite source");
    let tenant = ensure_self_hosted_tenant(&source, 1_000)
        .await
        .expect("a tenant");
    db_support::insert_authorized_key(&source, &"a".repeat(64), &KEY_BYTES, "laptop", None).await;
    sqlx::query(
        "INSERT INTO authorized_key_revocations (fingerprint, revoked_at_ms, revoked_by_fp, reason) \
         VALUES ($1, 2000, $1, 'lost')",
    )
    .bind("a".repeat(64))
    .execute(source.pool())
    .await
    .expect("a revocation");
    for ts in [10_i64, 20, 30] {
        insert_event(&source, ts, &tenant.dashboard_id)
            .await
            .expect("an event");
    }
    sqlx::query("DELETE FROM events WHERE ts = 20")
        .execute(source.pool())
        .await
        .expect("a gap in the ids");
    source.pool().close().await;
}

async fn insert_event(database: &CoordDb, ts: i64, dashboard_id: &str) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO events (kind, session_id, payload_json, ts, dashboard_id) \
         VALUES ('opened', 's1', '{}', $1, $2) RETURNING id",
    )
    .bind(ts)
    .bind(dashboard_id)
    .fetch_one(database.pool())
    .await
}

async fn count(database: &CoordDb, table: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
        .fetch_one(database.pool())
        .await
        .expect("a count")
}

#[tokio::test]
async fn every_row_lands_and_new_ids_continue_past_the_copied_ones() {
    let scratch = Scratch::new("copy");
    let Some(target_url) = scratch.target().await else {
        return;
    };
    seed_source(&scratch.source()).await;

    let report = copy_sqlite_to_postgres(&scratch.source(), &target_url, ExistingRows::Refuse)
        .await
        .expect("a copy into an empty database");
    let rows_of = |table: &str| {
        report
            .tables
            .iter()
            .find(|entry| entry.table == table)
            .map(|entry| entry.rows)
    };
    assert_eq!(rows_of("events"), Some(2));
    assert_eq!(rows_of("authorized_keys"), Some(1));
    assert_eq!(rows_of("authorized_key_revocations"), Some(1));
    assert_eq!(rows_of("accounts"), Some(1));

    let target = db::open(&DatabaseLocation::Postgres(target_url.clone()))
        .await
        .expect("the copied database opens as a coordinator would");
    let key: Vec<u8> = sqlx::query_scalar("SELECT public_key FROM authorized_keys")
        .fetch_one(target.pool())
        .await
        .expect("the copied key");
    assert_eq!(key, KEY_BYTES);
    let tenant = ensure_self_hosted_tenant(&target, 5_000)
        .await
        .expect("the copied tenant is the one tenant");
    assert_eq!(
        count(&target, "accounts").await,
        1,
        "no second account was created"
    );
    let next_id = insert_event(&target, 40, &tenant.dashboard_id)
        .await
        .expect("a new event after the copy");
    assert_eq!(
        next_id, 4,
        "the identity continues past the largest copied id"
    );
    target.pool().close().await;
}

#[tokio::test]
async fn a_target_with_rows_is_refused_unless_replaced_and_a_replace_does_not_double() {
    let scratch = Scratch::new("replace");
    let Some(target_url) = scratch.target().await else {
        return;
    };
    seed_source(&scratch.source()).await;
    copy_sqlite_to_postgres(&scratch.source(), &target_url, ExistingRows::Refuse)
        .await
        .expect("the first copy");

    let refused = copy_sqlite_to_postgres(&scratch.source(), &target_url, ExistingRows::Refuse)
        .await
        .expect_err("a second copy into the same database");
    assert!(
        matches!(&refused, TransferError::TargetNotEmpty { tables } if tables.contains("events")),
        "{refused}"
    );

    copy_sqlite_to_postgres(&scratch.source(), &target_url, ExistingRows::Replace)
        .await
        .expect("a replacing copy");
    let target = db::open(&DatabaseLocation::Postgres(target_url))
        .await
        .expect("the target");
    assert_eq!(count(&target, "events").await, 2);
    assert_eq!(count(&target, "authorized_keys").await, 1);
    target.pool().close().await;
}

#[tokio::test]
async fn a_missing_source_is_refused_without_creating_it() {
    let scratch = Scratch::new("missing");
    let refused = copy_sqlite_to_postgres(
        &scratch.source(),
        "postgres://unused@127.0.0.1:1/unused",
        ExistingRows::Refuse,
    )
    .await
    .expect_err("no source file");
    assert!(
        matches!(refused, TransferError::SourceMissing(_)),
        "{refused}"
    );
    assert!(
        !scratch.source().exists(),
        "the refusal must not create the file"
    );
}
