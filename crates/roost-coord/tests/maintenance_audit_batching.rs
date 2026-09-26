//! How the sweep bounds itself: ten thousand rows per statement, repeated until
//! the cutoff range is exhausted, and no statement that rewrites the file.
//!
//! The no-VACUUM claim is the reason this file reads page counts instead of
//! row counts. A retention sweep is the obvious place to add a VACUUM, and a
//! VACUUM on a live coordinator takes an EXCLUSIVE lock over the whole file
//! and rewrites it end to end -- which is an outage, bought for disk the
//! freelist already accounts for.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod maintenance_audit_support;

use maintenance_audit_support::{
    AuditFixture, NOW_MS, RETENTION_DAYS, SESSIONS_INPUT_PATH, days_ago,
};
use roost_coord::maintenance::audit_retention::{
    AUDIT_BATCH_SIZE, AuditSweepOptions, sweep_audit_log,
};

/// One row more than a single full batch, so a sweep that stopped after one
/// statement would leave exactly one row behind.
const BACKLOG: i64 = 10_001;

#[test]
fn the_batch_size_is_ten_thousand_rows_per_statement() {
    assert_eq!(AUDIT_BATCH_SIZE, 10_000);
    assert_eq!(BACKLOG, AUDIT_BATCH_SIZE + 1);
}

#[tokio::test]
async fn a_backlog_larger_than_one_batch_is_swept_to_the_end() {
    let fixture = AuditFixture::new("backlog").await;
    fixture
        .seed_bulk(days_ago(200), SESSIONS_INPUT_PATH, BACKLOG)
        .await;
    let fresh = fixture.seed(days_ago(1), "POST", SESSIONS_INPUT_PATH).await;

    let deleted = fixture.sweep().await;

    assert_eq!(
        deleted, BACKLOG as u64,
        "the loop runs until the range is exhausted"
    );
    assert_eq!(
        fixture.surviving_ids().await,
        vec![fresh],
        "the row inside the window is the one left"
    );
}

#[tokio::test]
async fn a_backlog_is_swept_in_several_statements_not_one_unbounded_delete() {
    let fixture = AuditFixture::new("small-batches").await;
    for _ in 0..5 {
        fixture
            .seed(days_ago(200), "POST", SESSIONS_INPUT_PATH)
            .await;
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

    assert_eq!(
        deleted, 5,
        "a short batch ends the range, a full one continues it"
    );
    assert!(fixture.paths().await.is_empty());
}

#[tokio::test]
async fn the_sweep_frees_pages_without_rewriting_the_file() {
    let fixture = AuditFixture::new("no-vacuum").await;
    fixture
        .seed_bulk(days_ago(200), SESSIONS_INPUT_PATH, BACKLOG)
        .await;

    let pages_before = fixture.pragma_i64("page_count").await;
    let free_before = fixture.pragma_i64("freelist_count").await;

    assert_eq!(fixture.sweep().await, BACKLOG as u64);

    assert_eq!(
        fixture.pragma_i64("page_count").await,
        pages_before,
        "a VACUUM would have rewritten the file down to its used pages"
    );
    let free_after = fixture.pragma_i64("freelist_count").await;
    assert!(
        free_after > free_before,
        "the deleted rows' pages go on the freelist for reuse, and the file stops growing: \
         {free_before} -> {free_after}"
    );
}
