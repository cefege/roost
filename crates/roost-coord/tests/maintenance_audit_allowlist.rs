//! Which audit_log rows the sweep may delete: one named method, matched on the
//! trailing path segment, and never on the method column.
//!
//! The allowlist is the whole safety argument for retention. Everything
//! authorization, pairing, deletion or lifecycle-related is deliberately absent
//! from it, because "when was this device authorised, and by whom" is a
//! question someone asks a year later and a deleted row cannot answer it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod maintenance_audit_support;

use maintenance_audit_support::{AuditFixture, SESSIONS_INPUT_PATH, days_ago};
use roost_coord::maintenance::audit_retention::AUDIT_SWEEP_METHODS;

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
            "/roost.v1.CoordinatorService/PairConfirm",
            "/roost.v1.CoordinatorService/PairApprove",
            "/roost.v1.CoordinatorService/AuthRedeemBrowser",
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
    fixture
        .seed(days_ago(200), "POST", SESSIONS_INPUT_PATH)
        .await;
    fixture
        .seed(
            days_ago(200),
            "POST",
            "/roost.worker.v1.WorkerService/SessionsInput",
        )
        .await;
    fixture.seed(days_ago(200), "POST", "/SessionsInput").await;

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
        .seed(
            days_ago(200),
            "SessionsInput",
            "/roost.v1.CoordinatorService/SessionsGet",
        )
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
