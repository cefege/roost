//! The startup-only cleanup of the pre-hardening backlog: anonymous successful
//! SPA and static reads, and nothing else.
//!
//! It is startup-only on purpose. Sweeping these on a schedule would mean
//! paying an INSERT per RPC to delete the row days later, which is the cost
//! the write-time skip in the auth interceptor already avoids.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod maintenance_audit_support;

use maintenance_audit_support::{AuditFixture, days_ago};
use roost_coord::maintenance::audit_retention::cleanup_anonymous_static_audit_log;

#[tokio::test]
async fn the_static_backlog_cleanup_touches_only_anonymous_successful_static_reads() {
    let fixture = AuditFixture::new("static").await;
    let anonymous_ok = fixture
        .seed_row(days_ago(1), "GET", "/index.html", 200, None)
        .await;
    let anonymous_moved = fixture
        .seed_row(days_ago(1), "GET", "/old", 301, None)
        .await;
    fixture
        .seed_row(days_ago(1), "HEAD", "/index.html", 200, None)
        .await;
    fixture
        .seed_row(days_ago(1), "GET", "/index.html", 200, Some("fp"))
        .await;
    fixture
        .seed_row(days_ago(1), "GET", "/api/db-export", 200, None)
        .await;
    fixture
        .seed_row(days_ago(1), "GET", "/api/workers", 200, None)
        .await;
    fixture
        .seed_row(days_ago(1), "GET", "/internal", 200, None)
        .await;
    fixture
        .seed_row(days_ago(1), "GET", "/internal/health", 200, None)
        .await;
    fixture.seed_row(days_ago(1), "GET", "/ws", 200, None).await;
    fixture
        .seed_row(days_ago(1), "GET", "/ws/coord-sync", 200, None)
        .await;
    fixture
        .seed_row(
            days_ago(1),
            "GET",
            "/roost.v1.CoordinatorService/SessionsGet",
            200,
            None,
        )
        .await;
    // A failed read and a non-GET verb on a static path are not backlog.
    fixture
        .seed_row(days_ago(1), "GET", "/broken", 500, None)
        .await;
    fixture
        .seed_row(days_ago(1), "DELETE", "/index.html", 200, None)
        .await;

    let deleted = cleanup_anonymous_static_audit_log(&fixture.database, None)
        .await
        .expect("the cleanup applies");

    assert_eq!(deleted, 3, "the three anonymous successful static reads");
    let kept = fixture.surviving_ids().await;
    assert!(
        !kept.contains(&anonymous_ok) && !kept.contains(&anonymous_moved),
        "a successful anonymous static read is what this exists to remove"
    );
    assert_eq!(kept.len(), 10, "everything else survives: {kept:?}");
}
