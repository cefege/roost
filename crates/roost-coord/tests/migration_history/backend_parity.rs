//! The invariants both database backends must enforce identically: the
//! tenancy triggers, the event id order, the boolean read the `Any` driver
//! cannot decode natively, and an id list longer than one `IN (...)` chunk.
//!
//! Runs on whichever backend `ROOST_TEST_DATABASE_URL` selects, so CI's
//! Postgres job and the default SQLite run assert the same contract.

use super::db_support;

use roost_coord::auth::authorized_keys::load_worker_facts;
use roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant;
use roost_coord::db::{CoordDb, IN_LIST_CHUNK};
use roost_coord::ui_state::fence::require_persisted_sessions;

/// A scratch directory and the database it owns, removed on drop.
struct Scratch {
    root: std::path::PathBuf,
    database: CoordDb,
}

impl Scratch {
    async fn new(label: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("roost-db-parity-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = db_support::open_test_database(&root)
            .await
            .expect("a migrated database");
        Self { root, database }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fingerprint(byte: char) -> String {
    std::iter::repeat_n(byte, 64).collect()
}

async fn insert_worker(
    database: &CoordDb,
    fp: &str,
    dashboard_id: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
         VALUES ($1, 'parity', 'linux', 1, 1, $2)",
    )
    .bind(fp)
    .bind(dashboard_id)
    .execute(database.pool())
    .await
    .map(|_| ())
}

async fn insert_session(
    database: &CoordDb,
    id: &str,
    worker_fp: &str,
    dashboard_id: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO sessions (id, worker_fp, channel, kind, cwd, status, created_at, dashboard_id) \
         VALUES ($1, $2, 1, 'shell', '/', 'open', 1, $3)",
    )
    .bind(id)
    .bind(worker_fp)
    .bind(dashboard_id)
    .execute(database.pool())
    .await
    .map(|_| ())
}

#[tokio::test]
async fn a_worker_without_a_dashboard_scope_is_refused_by_the_database() {
    let scratch = Scratch::new("worker-scope").await;
    ensure_self_hosted_tenant(&scratch.database, 1)
        .await
        .expect("the tenant");

    let refused = insert_worker(&scratch.database, &fingerprint('a'), None)
        .await
        .expect_err("an unscoped worker row is refused");
    assert!(
        refused
            .to_string()
            .contains("workers dashboard scope required"),
        "{refused}"
    );
}

#[tokio::test]
async fn a_session_scoped_away_from_its_worker_is_refused_by_the_database() {
    let scratch = Scratch::new("session-scope").await;
    let tenant = ensure_self_hosted_tenant(&scratch.database, 1)
        .await
        .expect("the tenant");
    let worker = fingerprint('b');
    insert_worker(&scratch.database, &worker, Some(&tenant.dashboard_id))
        .await
        .expect("a scoped worker");
    sqlx::query(
        "INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms) \
         VALUES ('dash-other', $1, 'other', 'Other', 'active', 1)",
    )
    .bind(&tenant.organization_id)
    .execute(scratch.database.pool())
    .await
    .expect("a second dashboard");

    let refused = insert_session(&scratch.database, "session-1", &worker, "dash-other")
        .await
        .expect_err("a session in another dashboard than its worker is refused");
    let message = refused.to_string();
    assert!(
        message.contains("session worker dashboard mismatch")
            || message.contains("sessions dashboard scope required"),
        "{message}"
    );
}

#[tokio::test]
async fn event_ids_are_strictly_increasing() {
    let scratch = Scratch::new("event-ids").await;
    let tenant = ensure_self_hosted_tenant(&scratch.database, 1)
        .await
        .expect("the tenant");
    let mut ids = Vec::new();
    for ts in 1..=3_i64 {
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO events (kind, payload_json, ts, dashboard_id) \
             VALUES ('snapshot', '{}', $1, $2) RETURNING id",
        )
        .bind(ts)
        .bind(&tenant.dashboard_id)
        .fetch_one(scratch.database.pool())
        .await
        .expect("an event row");
        ids.push(id);
    }
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
}

#[tokio::test]
async fn a_tombstoned_worker_reads_as_tombstoned() {
    let scratch = Scratch::new("tombstone").await;
    let tenant = ensure_self_hosted_tenant(&scratch.database, 1)
        .await
        .expect("the tenant");
    let (live, deleted) = (fingerprint('c'), fingerprint('d'));
    for fp in [&live, &deleted] {
        insert_worker(&scratch.database, fp, Some(&tenant.dashboard_id))
            .await
            .expect("a worker");
    }
    sqlx::query("UPDATE workers SET deleted_at_ms = 5 WHERE fp = $1")
        .bind(&deleted)
        .execute(scratch.database.pool())
        .await
        .expect("a tombstone");

    assert_eq!(
        load_worker_facts(&scratch.database, &live).await.unwrap(),
        (true, false)
    );
    assert_eq!(
        load_worker_facts(&scratch.database, &deleted)
            .await
            .unwrap(),
        (true, true)
    );
    assert_eq!(
        load_worker_facts(&scratch.database, &fingerprint('e'))
            .await
            .unwrap(),
        (false, false)
    );
}

#[tokio::test]
async fn an_id_list_longer_than_one_chunk_is_checked_whole() {
    let scratch = Scratch::new("long-id-list").await;
    let tenant = ensure_self_hosted_tenant(&scratch.database, 1)
        .await
        .expect("the tenant");
    let worker = fingerprint('f');
    insert_worker(&scratch.database, &worker, Some(&tenant.dashboard_id))
        .await
        .expect("a worker");
    let count = IN_LIST_CHUNK + 100;
    let ids: Vec<String> = (0..count)
        .map(|index| format!("session-{index:04}"))
        .collect();
    for id in &ids {
        insert_session(&scratch.database, id, &worker, &tenant.dashboard_id)
            .await
            .expect("a session");
    }

    require_persisted_sessions(scratch.database.pool(), &ids)
        .await
        .expect("every id spread across both chunks is found");

    let mut with_missing = ids.clone();
    with_missing.push("session-absent".to_owned());
    let refused = require_persisted_sessions(scratch.database.pool(), &with_missing)
        .await
        .expect_err("one absent id in the last chunk is refused");
    assert_eq!(refused.code, connectrpc::ErrorCode::NotFound);
}
