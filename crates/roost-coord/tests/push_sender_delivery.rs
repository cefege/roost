//! Web Push transport delivery: the 404/410 prune, the isolation of every
//! other failure, and the four-send concurrency ceiling.
//!
//! These are the three rules `push-sender.ts` exists to hold, and each is a
//! decision a tidier port loses silently. A prune that also fires on a 500
//! unsubscribes a whole fleet the first time a push provider has a bad
//! afternoon; an un-isolated failure drops sixteen notifications because the
//! first one failed; an unbounded fan-out turns one busy transition into a
//! self-inflicted outage.

// A test that cannot say what it expected is not a test. `expect` is denied
// outside `#[cfg(test)]`, and an integration test is its own crate, so the
// exemption has to be stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod push_fixture;

use std::sync::Arc;
use std::time::Duration;

use push_fixture::FakeTransport;
use push_fixture::{PUSH_ORIGIN, PushFixture, viewer_fp};
use roost_coord::push::sender::{
    PushDeliveryOptions, PushDeliveryResult, send_push_to_subscriptions,
};
use roost_coord::push::subscription_store::StoredSubscription;
use roost_coord::push::transport::{
    MAX_CONCURRENT_SENDS, PushTransportError, REQUEST_TIMEOUT, TTL_SECONDS,
};

/// Nine stored subscriptions, so the batch is wider than the ceiling.
fn wide_batch(count: usize) -> Vec<StoredSubscription> {
    (0..count)
        .map(|index| StoredSubscription {
            dashboard_id: "dash".to_owned(),
            viewer_fp: viewer_fp('a'),
            endpoint: format!("{PUSH_ORIGIN}/subscription-{index}"),
            p256dh: "abc".to_owned(),
            auth: "def".to_owned(),
            created_at_ms: index as i64,
        })
        .collect()
}

#[tokio::test]
async fn a_410_prunes_the_subscription_and_reports_it_as_expired() {
    let fixture = PushFixture::new("prune-410").await;
    let fp = fixture.fp();
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &fp,
            &format!("{PUSH_ORIGIN}/expired-secret"),
        )
        .await;
    let batch = wide_batch(1);
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
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &fp,
            &format!("{PUSH_ORIGIN}/gone-secret"),
        )
        .await;
    let batch = wide_batch(1);
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
        fixture
            .seed_subscription(&fixture.dashboard_id, &fp, &endpoint)
            .await;
        let transport =
            FakeTransport::failing(PushTransportError::with_status(status, "not a dead token"));

        let result = send_push_to_subscriptions(
            fixture.database().pool(),
            &wide_batch(1),
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
    fixture
        .seed_subscription(&fixture.dashboard_id, &fp, &endpoint)
        .await;
    let transport = FakeTransport::failing(PushTransportError::without_status("request timed out"));

    let result = send_push_to_subscriptions(
        fixture.database().pool(),
        &wide_batch(1),
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
    let batch = wide_batch(9);
    for subscription in &batch {
        fixture
            .seed_subscription(
                &fixture.dashboard_id,
                &subscription.viewer_fp,
                &subscription.endpoint,
            )
            .await;
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

#[tokio::test]
async fn at_most_four_deliveries_are_in_flight_at_once() {
    let fixture = PushFixture::new("ceiling").await;
    let batch = wide_batch(9);
    // The gate holds every attempt open until the test says go, so the peak is
    // whatever the sender actually started rather than whatever a sleep
    // happened to observe.
    let (transport, release) = FakeTransport::gated();

    let deliveries = tokio::spawn({
        let batch = batch.clone();
        let transport = Arc::clone(&transport);
        let pool = fixture.database().pool().clone();
        async move {
            send_push_to_subscriptions(
                &pool,
                &batch,
                r#"{"test":true}"#,
                PushDeliveryOptions::default(),
                &*transport,
            )
            .await
        }
    });

    // Wait until the ceiling is saturated, then let it go.
    let mut saturated = Vec::new();
    for _ in 0..200 {
        saturated.push(transport.peak_in_flight());
        if transport.peak_in_flight() >= MAX_CONCURRENT_SENDS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        transport.peak_in_flight(),
        MAX_CONCURRENT_SENDS,
        "the sender must saturate its four slots, not fewer"
    );
    release.send(true).expect("the gate receiver is still held");
    let result = deliveries.await.expect("the batch completes");

    assert_eq!(
        result,
        PushDeliveryResult {
            delivered: 9,
            expired: 0,
            failed: 0
        },
        "a bounded batch still delivers everything"
    );
    assert_eq!(
        transport.peak_in_flight(),
        MAX_CONCURRENT_SENDS,
        "the ceiling is four, and the ninth send waits for a slot"
    );
    assert_eq!(transport.attempted(), 9);
}

#[tokio::test]
async fn a_batch_narrower_than_the_ceiling_still_delivers_everything() {
    let fixture = PushFixture::new("narrow").await;
    let batch = wide_batch(2);
    let transport = FakeTransport::accepting();

    let result = send_push_to_subscriptions(
        fixture.database().pool(),
        &batch,
        r#"{"test":true}"#,
        PushDeliveryOptions::default(),
        transport.as_ref(),
    )
    .await;

    assert_eq!(result.delivered, 2);
    assert_eq!(
        transport.peak_in_flight(),
        2,
        "two subscriptions cannot use four slots"
    );
}

#[tokio::test]
async fn every_request_carries_the_ttl_the_ceiling_and_the_deduplication_topic() {
    let fixture = PushFixture::new("request-shape").await;
    let batch = wide_batch(1);
    let transport = FakeTransport::accepting();

    send_push_to_subscriptions(
        fixture.database().pool(),
        &batch,
        r#"{"sessionId":"s"}"#,
        PushDeliveryOptions {
            deduplication_token: Some("dedup-token-value"),
            is_current: None,
        },
        transport.as_ref(),
    )
    .await;

    let recorded = transport.deliveries();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].ttl_secs, u64::from(TTL_SECONDS));
    assert_eq!(recorded[0].timeout_ms, REQUEST_TIMEOUT.as_millis());
    assert_eq!(
        recorded[0].topic.as_deref(),
        Some("dedup-token-value"),
        "the RFC 8030 topic is what replaces a stale notification"
    );
    assert_eq!(recorded[0].body, r#"{"sessionId":"s"}"#);
}

#[tokio::test]
async fn a_superseded_transition_sends_nothing_at_all() {
    let fixture = PushFixture::new("superseded").await;
    let batch = wide_batch(9);
    let transport = FakeTransport::accepting();

    let result = send_push_to_subscriptions(
        fixture.database().pool(),
        &batch,
        r#"{"test":true}"#,
        PushDeliveryOptions {
            deduplication_token: None,
            is_current: Some(Arc::new(|| false)),
        },
        transport.as_ref(),
    )
    .await;

    assert_eq!(result, PushDeliveryResult::default());
    assert_eq!(
        transport.attempted(),
        0,
        "a transition that stopped being current must not reach the network"
    );
}

#[tokio::test]
async fn a_transition_superseded_mid_batch_stops_further_sends() {
    let fixture = PushFixture::new("superseded-mid").await;
    let batch = wide_batch(64);
    let (transport, release) = FakeTransport::gated();
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let fence: Arc<dyn Fn() -> bool + Send + Sync> = {
        let flag = Arc::clone(&flag);
        Arc::new(move || flag.load(std::sync::atomic::Ordering::SeqCst))
    };

    let deliveries = tokio::spawn({
        let batch = batch.clone();
        let transport = Arc::clone(&transport);
        let pool = fixture.database().pool().clone();
        async move {
            send_push_to_subscriptions(
                &pool,
                &batch,
                r#"{"test":true}"#,
                PushDeliveryOptions {
                    deduplication_token: None,
                    is_current: Some(Arc::clone(&fence)),
                },
                &*transport,
            )
            .await
        }
    });

    while transport.peak_in_flight() < MAX_CONCURRENT_SENDS {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // The transition is superseded while the first four are still in flight.
    flag.store(false, std::sync::atomic::Ordering::SeqCst);
    release.send(true).expect("the gate receiver is held");
    let _ = deliveries.await.expect("the batch completes");

    assert!(
        transport.attempted() < 64,
        "the fence must stop the remaining sends: {} of 64 attempted",
        transport.attempted()
    );
}

#[tokio::test]
async fn an_empty_batch_sends_nothing_and_never_touches_the_transport() {
    let fixture = PushFixture::new("empty").await;
    let transport = FakeTransport::accepting();

    let result = send_push_to_subscriptions(
        fixture.database().pool(),
        &[],
        r#"{"test":true}"#,
        PushDeliveryOptions::default(),
        transport.as_ref(),
    )
    .await;

    assert_eq!(result, PushDeliveryResult::default());
    assert_eq!(transport.attempted(), 0);
}
