//! Web Push delivery outcomes: which failure prunes a subscription and which
//! one is merely recorded.
//!
//! This is half of what `push-sender.ts` decides; the concurrency ceiling and
//! the supersession fence are in `push_sender_bounds.rs`. The split is along
//! the rule rather than along the file: a prune that also fires on a 500
//! unsubscribes a whole fleet the first time a push provider has a bad
//! afternoon, and an un-isolated failure drops sixteen notifications because
//! the first one failed.

// A test that cannot say what it expected is not a test. `expect` is denied
// outside `#[cfg(test)]`, and an integration test is its own crate, so the
// exemption has to be stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod push_fixture;

use push_fixture::FakeTransport;
use push_fixture::{PUSH_ORIGIN, PushFixture};
use roost_coord::push::sender::{
    PushDeliveryOptions, PushDeliveryResult, send_push_to_subscriptions,
};
use roost_coord::push::subscription_store::StoredSubscription;
use roost_coord::push::transport::PushTransportError;

/// One stored subscription, seeded into the database AND returned as the row
/// the sender will be handed.
///
/// The two halves have to be the same row. A test that seeds one endpoint and
/// delivers another exercises nothing: the sender's `DELETE` names the endpoint
/// it was asked about, finds no row, and the seeded row survives -- which looks
/// exactly like "410 did not prune" and is not.
async fn one_seeded_subscription(fixture: &PushFixture, endpoint: &str) -> StoredSubscription {
    fixture
        .seed_subscription(&fixture.dashboard_id, &fixture.fp(), endpoint)
        .await;
    StoredSubscription {
        dashboard_id: fixture.dashboard_id.clone(),
        viewer_fp: fixture.fp(),
        endpoint: endpoint.to_owned(),
        p256dh: "abc".to_owned(),
        auth: "def".to_owned(),
        created_at_ms: 1_000,
    }
}

/// Nine stored subscriptions, so the batch is wider than the ceiling.

#[tokio::test]
async fn a_410_prunes_the_subscription_and_reports_it_as_expired() {
    let fixture = PushFixture::new("prune-410").await;
    let fp = fixture.fp();
    let batch =
        vec![one_seeded_subscription(&fixture, &format!("{PUSH_ORIGIN}/expired-secret")).await];
    let transport = FakeTransport::failing(PushTransportError::with_status(410, "Gone"));

    let result = send_push_to_subscriptions(
        fixture.database().pool(),
        &batch,
        r#"{"test":true}"#,
        PushDeliveryOptions::default(),
        transport.as_ref(),
    )
    .await;

    assert_eq!(
        result,
        PushDeliveryResult {
            delivered: 0,
            expired: 1,
            failed: 0
        }
    );
    assert!(
        fixture.endpoints_for(&fp).await.is_empty(),
        "a 410 means the token is retired; the row must go"
    );
}

#[tokio::test]
async fn a_404_prunes_too_because_providers_do_not_distinguish_it_from_410() {
    let fixture = PushFixture::new("prune-404").await;
    let fp = fixture.fp();
    let batch =
        vec![one_seeded_subscription(&fixture, &format!("{PUSH_ORIGIN}/gone-secret")).await];
    let transport = FakeTransport::failing(PushTransportError::with_status(404, "Not Found"));

    let result = send_push_to_subscriptions(
        fixture.database().pool(),
        &batch,
        r#"{"test":true}"#,
        PushDeliveryOptions::default(),
        transport.as_ref(),
    )
    .await;

    assert_eq!(result.expired, 1);
    assert!(fixture.endpoints_for(&fp).await.is_empty());
}

#[tokio::test]
async fn every_other_status_is_recorded_and_the_row_survives() {
    // The distinction IS the point of `push-sender.ts`. Each of these is a
    // status a push provider returns while the subscription is perfectly
    // alive, and pruning on any of them would unsubscribe a working fleet.
    for status in [400_u16, 401, 403, 413, 429, 500, 502, 503] {
        let fixture = PushFixture::new(&format!("keep-{status}")).await;
        let fp = fixture.fp();
        let endpoint = format!("{PUSH_ORIGIN}/alive-{status}");
        let batch = vec![one_seeded_subscription(&fixture, &endpoint).await];
        let transport =
            FakeTransport::failing(PushTransportError::with_status(status, "not a dead token"));

        let result = send_push_to_subscriptions(
            fixture.database().pool(),
            &batch,
            r#"{"test":true}"#,
            PushDeliveryOptions::default(),
            transport.as_ref(),
        )
        .await;

        assert_eq!(
            result,
            PushDeliveryResult {
                delivered: 0,
                expired: 0,
                failed: 1
            },
            "{status} is a failure, not an expiry"
        );
        assert_eq!(
            fixture.endpoints_for(&fp).await,
            vec![endpoint],
            "{status} must not unsubscribe a working device"
        );
    }
}

#[tokio::test]
async fn a_failure_with_no_status_at_all_is_isolated_rather_than_pruning() {
    let fixture = PushFixture::new("keep-timeout").await;
    let fp = fixture.fp();
    let endpoint = format!("{PUSH_ORIGIN}/timed-out");
    let batch = vec![one_seeded_subscription(&fixture, &endpoint).await];
    let transport = FakeTransport::failing(PushTransportError::without_status("request timed out"));

    let result = send_push_to_subscriptions(
        fixture.database().pool(),
        &batch,
        r#"{"test":true}"#,
        PushDeliveryOptions::default(),
        transport.as_ref(),
    )
    .await;

    assert_eq!(result.failed, 1);
    assert_eq!(result.expired, 0);
    assert_eq!(fixture.endpoints_for(&fp).await, vec![endpoint]);
}

#[tokio::test]
async fn one_failing_endpoint_does_not_cost_the_batch_the_others() {
    let fixture = PushFixture::new("isolate").await;
    let mut batch = Vec::new();
    for index in 0..9 {
        batch.push(
            one_seeded_subscription(&fixture, &format!("{PUSH_ORIGIN}/subscription-{index}")).await,
        );
    }
    let transport = FakeTransport::failing(PushTransportError::with_status(500, "provider down"));

    let result = send_push_to_subscriptions(
        fixture.database().pool(),
        &batch,
        r#"{"test":true}"#,
        PushDeliveryOptions::default(),
        transport.as_ref(),
    )
    .await;

    // Every attempt is attempted and every failure is counted: a provider
    // outage must not read as "nothing to do".
    assert_eq!(result.failed, 9, "each failure is isolated and counted");
    assert_eq!(
        fixture.endpoints_for(&batch[0].viewer_fp).await.len(),
        9,
        "a 500 must not delete a single row"
    );
}
