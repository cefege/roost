//! `GET /version` through the mounted stack: unauthenticated, uncached, JSON
//! exactly `{"version":…,"git_sha":…}` from the build's own identity, answered
//! ahead of the SPA, and not an audited page. Workers poll this route to learn
//! which release to follow, so each of those properties is load-bearing.

use roost_host::{ProcessEnv, build_identity};

use super::middleware_support::{FixtureConfig, ListenerFixture};

/// The route answers with the build identity and nothing else, even when a
/// browser shell is being served for every other path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_answers_the_build_identity_ahead_of_the_spa() {
    let fixture = ListenerFixture::start(
        "version",
        FixtureConfig {
            serve_dist: true,
            ..FixtureConfig::default()
        },
    )
    .await;
    let identity = build_identity(&ProcessEnv::new());
    let expected = format!(
        r#"{{"version":"{}","git_sha":"{}"}}"#,
        identity.artifact_version, identity.build_sha
    );

    let response = fixture.get("/version");

    assert_eq!(response.status, 200);
    assert_eq!(response.body, expected, "the exact wire body");
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert_eq!(response.header("content-type"), Some("application/json"));
}

/// The route is a probe in every sense that matters to the fleet: a poll every
/// few minutes writes no audit row and needs no credential.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_needs_no_credential_and_writes_no_audit_row() {
    let fixture = ListenerFixture::start("version-audit", FixtureConfig::default()).await;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(fixture.services.db.pool())
        .await
        .expect("the audit count");

    for _ in 0..3 {
        assert_eq!(fixture.get("/version").status, 200);
    }

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(fixture.services.db.pool())
        .await
        .expect("the audit count");
    assert_eq!(after, before, "a version poll is not an audited request");
}
