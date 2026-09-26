// A shared test-support module is compiled once per test binary, and each
// of the four audit binaries drives a different subset of it. An item unused
// by one binary is not dead code -- the others use it -- and trimming it
// would make the fixture surface depend on which test is compiling.
#![allow(dead_code)]

//! The audit-retention fixture: a migrated database that removes itself, and
//! the row seeders the four retention test files share.
//!
//! Owned by the maintenance slice. One fixture rather than four copies, because
//! a test file that re-states its fixture is a file that has to be read twice.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use roost_coord::db::CoordDb;
use roost_coord::maintenance::audit_retention::{AuditSweepOptions, DAY_MS, sweep_audit_log};
use sqlx::AssertSqlSafe;

/// 2024-02-29T12:34:56.789Z, the same instant the backup tests name.
pub const NOW_MS: i64 = 1_709_210_096_789;
pub const RETENTION_DAYS: u64 = 90;
pub const SESSIONS_INPUT_PATH: &str = "/roost.v1.CoordinatorService/SessionsInput";

/// A migrated database in a directory that removes itself.
pub struct AuditFixture {
    pub database: CoordDb,
    root: PathBuf,
}

impl AuditFixture {
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!("roost-audit-{label}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        Self { database, root }
    }

    /// A row with a caller, a verb, a path and a status.
    pub async fn seed_row(
        &self,
        ts: i64,
        http_method: &str,
        path: &str,
        status: i64,
        caller_fp: Option<&str>,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO audit_log (ts, caller_fp, method, path, status) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(ts)
        .bind(caller_fp)
        .bind(http_method)
        .bind(path)
        .bind(status)
        .execute(self.database.pool())
        .await
        .expect("the row applies")
        .last_insert_rowid()
    }

    /// A successful POST from an anonymous caller, the shape of the rows the
    /// retention window is about.
    pub async fn seed(&self, ts: i64, http_method: &str, path: &str) -> i64 {
        self.seed_row(ts, http_method, path, 200, None).await
    }

    /// `rows` rows in one statement, because a backlog test that inserts ten
    /// thousand rows one at a time is testing SQLite's insert speed.
    pub async fn seed_bulk(&self, ts: i64, path: &str, rows: i64) {
        sqlx::query(
            "WITH RECURSIVE counter(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM counter WHERE i < ?) \
             INSERT INTO audit_log (ts, caller_fp, method, path, status) \
             SELECT ?, 'fp', 'POST', ?, 200 FROM counter",
        )
        .bind(rows)
        .bind(ts)
        .bind(path)
        .execute(self.database.pool())
        .await
        .expect("the bulk insert applies");
    }

    /// The paths that survived, in insertion order.
    pub async fn paths(&self) -> Vec<String> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT path FROM audit_log ORDER BY id")
            .fetch_all(self.database.pool())
            .await
            .expect("the remaining rows");
        rows.into_iter().map(|row| row.0).collect()
    }

    /// The ids that survived, in insertion order.
    pub async fn surviving_ids(&self) -> Vec<i64> {
        let rows: Vec<(i64,)> = sqlx::query_as("SELECT id FROM audit_log ORDER BY id")
            .fetch_all(self.database.pool())
            .await
            .expect("the remaining ids");
        rows.into_iter().map(|row| row.0).collect()
    }

    /// One sweep at the pinned clock, on the default batch size.
    pub async fn sweep(&self) -> u64 {
        sweep_audit_log(
            &self.database,
            AuditSweepOptions {
                retention_days: RETENTION_DAYS,
                now_ms: Some(NOW_MS),
                batch_size: None,
            },
        )
        .await
        .expect("the sweep applies")
    }

    pub async fn pragma_i64(&self, name: &str) -> i64 {
        let sql = format!("PRAGMA {name}");
        sqlx::query_scalar(AssertSqlSafe(sql))
            .fetch_one(self.database.pool())
            .await
            .expect("the pragma answers")
    }
}

impl Drop for AuditFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn days_ago(days: i64) -> i64 {
    NOW_MS - days * DAY_MS
}
