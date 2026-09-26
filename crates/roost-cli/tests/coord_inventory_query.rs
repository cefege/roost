//! The read-only coordinator-database read behind `roost status`'s fleet rows,
//! against a real SQLite file with the real schema columns. The query is the
//! one thing in the status group that can be wrong in a way no string assertion
//! catches — a renamed column, a missing projection, a wrong join — and it can
//! be wrong silently, rendering an empty roster on a healthy fleet.
//!
//! Two shapes are covered because both exist in the field: a current
//! coordinator with both projection columns, and an older one with neither,
//! which must render the same rows with the projections reported as absent
//! rather than failing the whole readout.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use roost_cli::status::inventory::{InventoryError, worker_inventory};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Executor, SqlitePool};

const NOW: i64 = 1_781_900_000_000;
const FRESH: i64 = 5_000;
const STALE: i64 = 600_000;

/// One file per (test, schema shape). The tests run in parallel, and a shared
/// file would have them truncating each other's schema mid-run — which looks
/// exactly like a broken query and is not one.
async fn database(case: &str, columns: &str) -> (PathBuf, SqlitePool) {
    // The schema is a fixture, so it is built as a literal per shape rather
    // than interpolated: a table column list is not a bind parameter, and a
    // test that assembled SQL at runtime would be testing its own assembly.
    let path = std::env::temp_dir().join(format!(
        "roost-cli-inventory-{}-{case}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    let projections = if columns.is_empty() {
        ""
    } else {
        ", keeper_runtime_json TEXT, terminal_core_capacity_json TEXT"
    };
    let schema = format!(
        "CREATE TABLE workers (
            fp TEXT NOT NULL,
            label TEXT NOT NULL,
            os TEXT NOT NULL,
            git_sha TEXT,
            reachable_addr TEXT,
            last_seen_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER{projections}
        )"
    );
    sqlx::query(AssertSqlSafe(schema))
        .execute(&pool)
        .await
        .unwrap();
    pool.execute(
        "CREATE TABLE sessions (id TEXT NOT NULL, worker_fp TEXT NOT NULL, status TEXT NOT NULL)",
    )
    .await
    .unwrap();
    (path, pool)
}

async fn seed(pool: &SqlitePool, columns: &str) {
    if columns.is_empty() {
        pool.execute(
            "INSERT INTO workers (fp, label, os, git_sha, reachable_addr, last_seen_ms, deleted_at_ms) VALUES
                ('fp-fresh', 'studio', 'darwin', 'abc123', '100.64.0.7', 1781899995000, null),
                ('fp-stale', 'laptop', 'linux', null, null, 1781899400000, null),
                ('fp-gone', 'retired', 'linux', null, null, 1781900000000, 1000)",
        )
        .await
        .unwrap();
    } else {
        sqlx::query(
            "INSERT INTO workers (fp, label, os, git_sha, reachable_addr, last_seen_ms,
                                  deleted_at_ms, keeper_runtime_json, terminal_core_capacity_json) VALUES
                ('fp-fresh', 'studio', 'darwin', 'abc123', '100.64.0.7', 1781899995000, null, ?1, ?2),
                ('fp-stale', 'laptop', 'linux', null, null, 1781899400000, null, null, null),
                ('fp-gone', 'retired', 'linux', null, null, 1781900000000, 1000, null, null)",
        )
        .bind(keeper_runtime_json())
        .bind(capacity_json())
        .execute(pool)
        .await
        .unwrap();
    }
    pool.execute(
        "INSERT INTO sessions (id, worker_fp, status) VALUES
            ('sess-b', 'fp-fresh', 'open'),
            ('sess-a', 'fp-fresh', 'open'),
            ('sess-c', 'fp-fresh', 'closed'),
            ('sess-d', 'fp-stale', 'open')",
    )
    .await
    .unwrap();
}

fn keeper_runtime_json() -> String {
    serde_json::json!({
        "schema_version": 1,
        "running_contract": {
            "protocol_version": 3,
            "supported_features": [],
            "required_features": [],
            "implementation_digest": "a".repeat(64),
            "platform": "linux",
            "arch": "x86_64",
            "build_sha": "abc123"
        },
        "keeper_pid": 99,
        "keeper_epoch": "6f1c2d3e-4a5b-4c6d-8e9f-0a1b2c3d4e5f",
        "channel_count": 0,
        "binding_digest": roost_protocol::keeper_update::KEEPER_EMPTY_BINDING_DIGEST,
        "reconciled_at_ms": NOW - 1_000
    })
    .to_string()
}

fn capacity_json() -> String {
    serde_json::json!({
        "used": 2,
        "pending": 1,
        "capacity": 8,
        "estimated_reserved_bytes": 20 * 1024 * 1024,
        "effective_memory_ceiling_bytes": 1024 * 1024 * 1024,
        "boot_rss_bytes": 50 * 1024 * 1024,
        "overcommit_count": 0,
        "refusal_count": 2
    })
    .to_string()
}

fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

#[tokio::test]
async fn reads_the_roster_and_derives_staleness_from_one_clock() {
    let (path, pool) = database("full", "keeper_runtime_json, terminal_core_capacity_json").await;
    seed(&pool, "keeper_runtime_json, terminal_core_capacity_json").await;
    pool.close().await;

    let workers = worker_inventory(&path, NOW).await.unwrap();
    remove(&path);

    // The soft-deleted row is not in the roster: a machine the operator retired
    // must not appear as a fleet member it can no longer reach.
    assert_eq!(workers.len(), 2);
    let fresh = workers.iter().find(|w| w.label == "studio").unwrap();
    assert_eq!(fresh.fingerprint, "fp-fresh");
    assert_eq!(fresh.age_ms, FRESH);
    assert!(!fresh.stale);
    assert_eq!(fresh.coordinator_open_session_ids, vec!["sess-a", "sess-b"]);
    assert_eq!(fresh.keeper_runtime.as_ref().unwrap().keeper_pid, 99);
    assert_eq!(fresh.terminal_core_capacity.as_ref().unwrap().used, 2);

    let stale = workers.iter().find(|w| w.label == "laptop").unwrap();
    assert_eq!(stale.age_ms, STALE);
    assert!(stale.stale);
    assert!(stale.keeper_runtime.is_none());
    assert_eq!(stale.coordinator_open_session_ids, vec!["sess-d"]);
}

#[tokio::test]
async fn a_coordinator_without_the_projection_columns_still_renders_its_roster() {
    // The columns arrived in a later release. Selecting them unconditionally
    // would make `roost status` fail outright against an older install, which
    // is the one machine whose fleet most needs looking at.
    let (path, pool) = database("bare", "").await;
    seed(&pool, "").await;
    pool.close().await;

    let workers = worker_inventory(&path, NOW).await.unwrap();
    remove(&path);

    assert_eq!(workers.len(), 2);
    assert!(workers.iter().all(|worker| worker.keeper_runtime.is_none()));
    assert!(
        workers
            .iter()
            .all(|worker| worker.terminal_core_capacity.is_none())
    );
}

#[tokio::test]
async fn a_database_that_is_not_there_is_an_error_and_not_an_empty_fleet() {
    let missing = std::env::temp_dir().join("roost-cli-no-such-coordinator.db");
    remove(&missing);
    let error = worker_inventory(&missing, NOW).await.unwrap_err();
    assert!(matches!(error, InventoryError::Missing(_)));
    assert!(error.to_string().contains("coordinator database not found"));
}

#[tokio::test]
async fn a_projection_that_does_not_parse_is_reported_as_absent() {
    // One machine's malformed column must not cost the other machine its row.
    let (path, pool) = database(
        "malformed",
        "keeper_runtime_json, terminal_core_capacity_json",
    )
    .await;
    pool.execute(
        "INSERT INTO workers (fp, label, os, git_sha, reachable_addr, last_seen_ms, \
         deleted_at_ms, keeper_runtime_json, terminal_core_capacity_json) \
         VALUES ('fp-bad', 'broken', 'linux', null, null, 1, null, 'not json', '{')",
    )
    .await
    .unwrap();
    pool.close().await;

    let workers = worker_inventory(&path, NOW).await.unwrap();
    remove(&path);
    assert_eq!(workers.len(), 1);
    assert!(workers[0].keeper_runtime.is_none());
    assert!(workers[0].terminal_core_capacity.is_none());
}
