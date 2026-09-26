//! The audit_log sweep's promises: one method ages out, the match is on the
//! trailing path segment and never on the method column, the window is epoch
//! milliseconds, the work is batched, and the file is never rewritten.
//!
//! The last one is the reason this file has a test that looks at page counts.
//! A retention sweep is the obvious place to add a VACUUM, and a VACUUM on a
//! live coordinator takes an EXCLUSIVE lock over the whole file and rewrites
//! it end to end, which is an outage.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use roost_coord::db::CoordDb;
use roost_coord::maintenance::audit_retention::{
    AUDIT_BATCH_SIZE, AUDIT_SWEEP_METHODS, AuditSweepOptions, DAY_MS,
    cleanup_anonymous_static_audit_log, sweep_audit_log,
};

/// 2024-02-29T12:34:56.789Z, the same instant the backup tests name.
const NOW_MS: i64 = 1_709_210_096_789;
const RETENTION_DAYS: u64 = 90;
const SESSIONS_INPUT_PATH: &str = "/roost.v1.CoordinatorService/SessionsInput";

struct AuditFixture {
    database: CoordDb,
    root: PathBuf,
}

impl AuditFixture {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!("roost-audit-{label}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        Self { database, root }
    }

    async fn seed(&self, ts: i64, http_method: &str, path: &str) -> i64 {
        self.seed_with_caller(ts, http_method, path, None).await
    }

    async fn seed_with_caller(
        &self,
        ts: i64,
        http_method: &str,
        path: &str,
        caller_fp: Option<&str>,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO audit_log (ts, caller_fp, method, path, status) \
             VALUES (?, ?, ?, ?, 200)",
        )
        .bind(ts)
        .bind(caller_fp)
        .bind(http_method)
        .bind(path)
        .execute(self.database.pool())
        .await
        .expect("the row applies")
        .last_insert_rowid()
    }

    async fn seed_bulk(&self, ts: i64, path: &str, rows: i64) {
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

    async fn paths(&self) -> Vec<String> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT path FROM audit_log ORDER BY id")
            .fetch_all(self.database.pool())
            .await
            .expect("the remaining rows");
        rows.into_iter().map(|row| row.0).collect()
    }

    async fn sweep(&self) -> u64 {
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

    async fn pragma_i64(&self, name: &str) -> i64 {
        let sql = format!("PRAGMA {name}");
        sqlx::query_scalar(&sql)
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

fn days_ago(days: i64) -> i64 {
    NOW_MS - days * DAY_MS
}

// ── the allowlist ──────────────────────────────────────────────────────────

#[test]
fn the_allowlist_is_one_method_and_is_named_as_a_list() {
    assert_eq!(
        AUDIT_SWEEP_METHODS,
        ["SessionsInput"],
        "the forensic rows -- PairConfirm, PairApprove, SessionsSpawn -- are not here"
    );
}

#[tokio::test]
async fn only_sessions_input_ages_out() {
    let fixture = AuditFixture::new("allowlist").await;
    for path in [
        SESSIONS_INPUT_PATH,
        "/roost.v1.CoordinatorService/PairConfirm",
        "/roost.v1.CoordinatorService/PairApprove",
        "/roost.v1.CoordinatorService/AuthRedeemBrowser",
        "/roost.v1.CoordinatorService/SessionsKill",
        "/roost.v1.CoordinatorService/SessionsSpawn",
        "/roost.v1.CoordinatorService/WorkersDelete",
        "/roost.v1.CoordinatorService/WorkspacesDelete",
    ] {
        fixture.seed(days_ago(365), "POST", path).await;
    }

    assert_eq!(fixture.sweep().await, 1);
    assert_eq!(
        fixture.paths().await,
        [
            "/roost.v1.CoordinatorService/AuthRedeemBrowser",
            "/roost.v1.CoordinatorService/PairApprove",
            "/roost.v1.CoordinatorService/PairConfirm",
            "/roost.v1.CoordinatorService/SessionsKill",
            "/roost.v1.CoordinatorService/SessionsSpawn",
            "/roost.v1.CoordinatorService/WorkersDelete",
            "/roost.v1.CoordinatorService/WorkspacesDelete",
        ],
        "'when was this device authorised, and by whom' survives a year"
    );
}

#[tokio::test]
async fn the_trailing_segment_matches_however_the_service_is_spelled() {
    let fixture = AuditFixture::new("trailing").await;
    fixture.seed(days_ago(200), "POST", SESSIONS_INPUT_PATH).await;
    fixture
        .seed(days_ago(200), "POST", "/roost.worker.v1.WorkerService/SessionsInput")
        .await;
    fixture
        .seed(days_ago(200), "POST", "/SessionsInput")
        .await;

    assert_eq!(
        fixture.sweep().await,
        3,
        "the service prefix varies across proto packages, so the match is on the last segment"
    );
    assert!(fixture.paths().await.is_empty());
}

#[tokio::test]
async fn a_name_that_only_ends_in_sessions_input_is_not_a_sessions_input_row() {
    let fixture = AuditFixture::new("near-miss").await;
    for path in [
        "/roost.v1.SessionsInput",
        "/roost.v1.CoordinatorService/SessionsInputExtra",
        "/roost.v1.CoordinatorService/SessionsInputs",
        "/roost.v1.CoordinatorService/Sessions",
    ] {
        fixture.seed(days_ago(200), "POST", path).await;
    }

    assert_eq!(fixture.sweep().await, 0);
    assert_eq!(fixture.paths().await.len(), 4);
}

#[tokio::test]
async fn the_method_column_is_never_what_the_sweep_matches() {
    let fixture = AuditFixture::new("method-column").await;
    // The interceptor writes the literal HTTP verb into `method`, so a sweep
    // that filtered on that column would delete nothing at all -- or, worse,
    // whatever someone later put there.
    fixture
        .seed(days_ago(200), "SessionsInput", "/roost.v1.CoordinatorService/SessionsGet")
        .await;
    fixture
        .seed(days_ago(200), "HEAD", SESSIONS_INPUT_PATH)
        .await;

    assert_eq!(
        fixture.sweep().await,
        1,
        "the row is selected by its path, whatever the verb column says"
    );
    assert_eq!(
        fixture.paths().await,
        ["/roost.v1.CoordinatorService/SessionsGet"]
    );
}

// ── the window ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_window_is_epoch_milliseconds_not_seconds() {
    let fixture = AuditFixture::new("milliseconds").await;
    // A seconds-scale timestamp for "now". A sweep computed in seconds would
    // leave this row alone, because it would be exactly at its own cutoff.
    let seconds_scale = fixture.seed(NOW_MS / 1000, "POST", SESSIONS_INPUT_PATH).await;
    // A millisecond-scale row a second old.
    let fresh = fixture.seed(NOW_MS - 1000, "POST", SESSIONS_INPUT_PATH).await;
    // Exactly at the cutoff, and a millisecond inside it.
    let at_cutoff = fixture
        .seed(NOW_MS - i64::try_from(RETENTION_DAYS).expect("90 days") * DAY_MS, "POST", SESSIONS_INPUT_PATH)
        .await;
    let inside = fixture
        .seed(
            NOW_MS - i64::try_from(RETENTION_DAYS).expect("90 days") * DAY_MS - 1,
            "POST",
            SESSIONS_INPUT_PATH,
        )
        .await;

    assert_eq!(fixture.sweep().await, 2);
    assert_eq!(
        fixture.paths().await.len(),
        2,
        "a row at the cutoff is kept: the comparison is strictly older-than"
    );
    let kept: Vec<i64> = sqlx::query_as("SELECT id FROM audit_log ORDER BY id")
        .fetch_all(fixture.database.pool())
        .await
        .expect("the remaining ids");
    assert_eq!(kept, vec![fresh, at_cutoff]);
    assert_ne!(kept[0], seconds_scale);
    assert_ne!(kept[0], inside);
}

// ── batching ───────────────────────────────────────────────────────────────

#[test]
fn the_batch_size_is_ten_thousand_rows_per_statement() {
    assert_eq!(AUDIT_BATCH_SIZE, 10_000);
    assert_eq!(DAY_MS, 24 * 60 * 60 * 1000);
}

#[tokio::test]
async fn a_backlog_larger_than_one_batch_is_swept_to_the_end() {
    let fixture = AuditFixture::new("backlog").await;
    let bulk = 10_001;
    fixture.seed_bulk(days_ago(200), SESSIONS_INPUT_PATH, bulk).await;
    let fresh = fixture.seed(days_ago(1), "POST", SESSIONS_INPUT_PATH).await;

    let deleted = fixture.sweep().await;

    assert_eq!(deleted, bulk, "the loop runs until the range is exhausted");
    assert_eq!(fixture.paths().await.len(), 1);
    let kept: Vec<i64> = sqlx::query_as("SELECT id FROM audit_log")
        .fetch_all(fixture.database.pool())
        .await
        .expect("the remaining ids");
    assert_eq!(kept, vec![fresh], "the row inside the window is the one left");
}

#[tokio::test]
async fn a_backlog_is_swept_in_several_statements_not_one_unbounded_delete() {
    let fixture = AuditFixture::new("small-batches").await;
    for _ in 0..5 {
        fixture.seed(days_ago(200), "POST", SESSIONS_INPUT_PATH).await;
    }

    let deleted = sweep_audit_log(
        &fixture.database,
        AuditSweepOptions {
            retention_days: RETENTION_DAYS,
            now_ms: Some(NOW_MS),
            batch_size: Some(2),
        },
    )
    .await
    .expect("the sweep applies");

    assert_eq!(deleted, 5, "a short batch ends the range, a full one continues it");
    assert!(fixture.paths().await.is_empty());
}

// ── no VACUUM ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_sweep_frees_pages_without_rewriting_the_file() {
    let fixture = AuditFixture::new("no-vacuum").await;
    fixture.seed_bulk(days_ago(200), SESSIONS_INPUT_PATH, 10_001).await;

    let pages_before = fixture.pragma_i64("page_count").await;
    let free_before = fixture.pragma_i64("freelist_count").await;

    assert_eq!(fixture.sweep().await, 10_001);

    let pages_after = fixture.pragma_i64("page_count").await;
    let free_after = fixture.pragma_i64("freelist_count").await;
    assert_eq!(
        pages_after, pages_before,
        "a VACUUM would have rewritten the file down to its used pages"
    );
    assert!(
        free_after > free_before,
        "the deleted rows' pages go on the freelist for reuse, and the file stops growing: \
         {free_before} -> {free_after}"
    );
}

// ── the startup-only static backlog cleanup ────────────────────────────────

#[tokio::test]
async fn the_static_backlog_cleanup_touches_only_anonymous_successful_static_reads() {
    let fixture = AuditFixture::new("static").await;
    let anonymous_ok = fixture.seed_with_caller(days_ago(1), "GET", "/index.html", None).await;
    let anonymous_moved = fixture.seed_with_caller(days_ago(1), "GET", "/old", None).await;
    fixture.seed_with_caller(days_ago(1), "HEAD", "/index.html", None).await;
    fixture
        .seed_with_caller(days_ago(1), "GET", "/index.html", Some("fp"))
        .await;
    fixture.seed(days_ago(1), "GET", "/api/db-export").await;
    fixture.seed(days_ago(1), "GET", "/api/workers").await;
    fixture.seed(days_ago(1), "GET", "/internal").await;
    fixture.seed(days_ago(1), "GET", "/internal/health").await;
    fixture.seed(days_ago(1), "GET", "/ws").await;
    fixture.seed(days_ago(1), "GET", "/ws/coord-sync").await;
    fixture
        .seed(days_ago(1), "GET", "/roost.v1.CoordinatorService/SessionsGet")
        .await;
    fixture.seed(days_ago(1), "GET", "/broken").await;
    fixture
        .seed(days_ago(1), "DELETE", "/index.html")
        .await;

    let deleted = cleanup_anonymous_static_audit_log(&fixture.database, None)
        .await
        .expect("the cleanup applies");

    assert_eq!(deleted, 2);
    let kept: Vec<i64> = sqlx::query_as("SELECT id FROM audit_log ORDER BY id")
        .fetch_all(fixture.database.pool())
        .await
        .expect("the remaining ids");
    assert!(
        !kept.contains(&anonymous_ok) && !kept.contains(&anonymous_moved),
        "a successful anonymous static read is what this exists to remove"
    );
    assert_eq!(kept.len(), 10);
    let free_after = fixture.pragma_i64("freelist_count").await;
    assert_eq!(
        free_after, 0,
        "and it does not rewrite the file either"
    );
}
