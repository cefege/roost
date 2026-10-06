//! The orchestrator probes through the mounted stack: `/healthz` answers while
//! the process serves, `/readyz` only while it should take traffic, and neither
//! is mistaken for a page, nor leaves an audit row behind. A submodule of the
//! admission-stack binary, whose fixture and crate-level allows it shares.

use std::sync::atomic::Ordering;

use super::middleware_support::{FixtureConfig, ListenerFixture};

async fn audit_rows(fixture: &ListenerFixture) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(fixture.services.db.pool())
        .await
        .expect("the audit count")
}

/// A served SPA would answer any GET with `index.html`; the probes are routes
/// of their own, ahead of it, and answer a body a probe can read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_probes_answer_ahead_of_the_spa_and_write_no_audit_row() {
    let fixture = ListenerFixture::start(
        "probes",
        FixtureConfig {
            serve_dist: true,
            ..FixtureConfig::default()
        },
    )
    .await;
    let before = audit_rows(&fixture).await;

    for _ in 0..3 {
        let live = fixture.get("/healthz");
        assert_eq!((live.status, live.body.as_str()), (200, "ok"));
        let ready = fixture.get("/readyz");
        assert_eq!((ready.status, ready.body.as_str()), (200, "ready"));
    }

    assert_eq!(
        audit_rows(&fixture).await,
        before,
        "a probe is not an audited request"
    );
}

/// Once shutdown raises the draining flag, readiness is withdrawn while
/// liveness still answers: the orchestrator stops sending traffic without
/// concluding the process is dead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn draining_withdraws_readiness_but_not_liveness() {
    let fixture = ListenerFixture::start("draining", FixtureConfig::default()).await;
    fixture.draining.store(true, Ordering::Release);

    let ready = fixture.get("/readyz");
    assert_eq!((ready.status, ready.body.as_str()), (503, "draining"));
    let live = fixture.get("/healthz");
    assert_eq!(live.status, 200);
}
