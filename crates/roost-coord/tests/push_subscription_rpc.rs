//! The three Web Push Connect methods over a real migrated database: the
//! per-device cap, the endpoint and key admission rules, and the push-disabled
//! state.
//!
//! The cap is the load-bearing thing here. v2 enforces it inside the
//! `INSERT ... SELECT ... WHERE` rather than counting and then inserting
//! (`apps/coord/src/push/handlers-push.ts:88-109`), because a count-then-insert
//! has a window between the two decisions that a device opening two tabs walks
//! straight into. The race case at the bottom is what pins that shape: it fails
//! against a count-then-insert and passes against the single statement.

// A test that cannot say what it expected is not a test. `expect` is denied
// outside `#[cfg(test)]`, and an integration test is its own crate, so the
// exemption has to be stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod push_fixture;

use connectrpc::ErrorCode;
use push_fixture::{PUSH_ORIGIN, PushFixture, viewer_fp};
use roost_coord::push::rpc::{
    handle_push_get_config, handle_push_subscribe, handle_push_unsubscribe,
};
use roost_coord::push::subscription_store::MAX_SUBSCRIPTIONS_PER_DEVICE;
use roost_proto::{PushGetConfigRequest, PushSubscribeRequest, PushUnsubscribeRequest};

/// A subscribe request with a well-formed endpoint and keys.
fn subscribe_request(endpoint: &str) -> PushSubscribeRequest {
    PushSubscribeRequest {
        endpoint: endpoint.to_owned(),
        p256dh: "abc".to_owned(),
        auth: "def".to_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn push_get_config_reports_the_vapid_public_key_when_push_is_configured() {
    let fixture = PushFixture::new("config-on").await;

    let response = handle_push_get_config(
        &fixture.core,
        &fixture.caller,
        PushGetConfigRequest::default(),
    )
    .await
    .expect("a paired device may read the config");

    assert!(response.body.available);
    assert!(
        !response.body.vapid_public_key_b64.is_empty(),
        "an available Push must hand the browser a key to subscribe with"
    );
    // 65 bytes of uncompressed P-256 point, base64url.
    assert_eq!(
        roost_host::b64url_decode(&response.body.vapid_public_key_b64)
            .expect("base64url")
            .len(),
        65
    );
}

#[tokio::test]
async fn a_browser_never_sees_a_vapid_key_until_push_is_enabled() {
    let fixture = PushFixture::build(
        "config-off",
        Vec::new(),
        push_fixture::production_generator(),
    )
    .await;

    let response = handle_push_get_config(
        &fixture.core,
        &fixture.caller,
        PushGetConfigRequest::default(),
    )
    .await
    .expect("the method answers either way");

    assert!(!response.body.available);
    assert!(
        response.body.vapid_public_key_b64.is_empty(),
        "a deployment with no push provider must not publish an identity"
    );
    // The row is not created either: a coordinator that never enables Push must
    // not accumulate a VAPID identity it will never use.
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT value FROM app_settings WHERE key = 'push.vapid'")
            .fetch_all(fixture.database().pool())
            .await
            .expect("the settings read");
    assert!(rows.is_empty(), "no VAPID row: {rows:?}");
}

#[tokio::test]
async fn an_unpaired_caller_cannot_read_the_push_config() {
    let fixture = PushFixture::new("config-auth").await;
    let worker = push_fixture::browser_caller(&viewer_fp('a'), "not-an-account");
    let mut caller = worker.clone();
    caller.principal = roost_coord::auth::principal::Principal::Worker {
        fingerprint: viewer_fp('c'),
        label: "worker".to_owned(),
    };

    let error = handle_push_get_config(&fixture.core, &caller, PushGetConfigRequest::default())
        .await
        .expect_err("a worker is not a browser device");

    assert_eq!(error.code, ErrorCode::Unauthenticated);
    assert_eq!(
        error.message.as_deref().unwrap_or_default(),
        "authentication required"
    );
}

#[tokio::test]
async fn a_device_may_hold_exactly_four_endpoints_and_the_fifth_is_refused() {
    let fixture = PushFixture::new("cap").await;
    let fp = fixture.fp();

    for index in 0..MAX_SUBSCRIPTIONS_PER_DEVICE {
        handle_push_subscribe(
            &fixture.core,
            &fixture.caller,
            subscribe_request(&format!("{PUSH_ORIGIN}/subscription-{index}")),
        )
        .await
        .unwrap_or_else(|error| panic!("subscription {index} is within the cap: {error}"));
    }
    assert_eq!(
        fixture.endpoints_for(&fp).await.len(),
        MAX_SUBSCRIPTIONS_PER_DEVICE
    );

    let refused = handle_push_subscribe(
        &fixture.core,
        &fixture.caller,
        subscribe_request(&format!("{PUSH_ORIGIN}/subscription-overflow")),
    )
    .await
    .expect_err("a fifth distinct endpoint is over the cap");

    assert_eq!(
        refused.code,
        ErrorCode::ResourceExhausted,
        "the cap is a 429, not a malformed request"
    );
    let message = refused.message.as_deref().unwrap_or_default().to_owned();
    assert!(
        message.contains(&MAX_SUBSCRIPTIONS_PER_DEVICE.to_string()),
        "the refusal must name the cap so a support report is actionable: {message}"
    );
    assert_eq!(
        fixture.endpoints_for(&fp).await.len(),
        MAX_SUBSCRIPTIONS_PER_DEVICE,
        "a refused subscribe must leave the device's rows alone"
    );
}

#[tokio::test]
async fn re_subscribing_a_held_endpoint_refreshes_it_at_the_cap() {
    let fixture = PushFixture::new("cap-refresh").await;
    let fp = fixture.fp();
    let first = format!("{PUSH_ORIGIN}/subscription-held");
    for index in 0..MAX_SUBSCRIPTIONS_PER_DEVICE {
        handle_push_subscribe(
            &fixture.core,
            &fixture.caller,
            subscribe_request(&format!("{PUSH_ORIGIN}/subscription-{index}")),
        )
        .await
        .expect("within the cap");
    }

    // A browser rotating its keys re-subscribes the SAME endpoint. That is the
    // `EXISTS` arm of the upsert and it must be admitted at the cap, or a
    // device that has accumulated three dead endpoints could never rotate the
    // fourth one's keys.
    handle_push_subscribe(
        &fixture.core,
        &fixture.caller,
        PushSubscribeRequest {
            endpoint: first.clone(),
            p256dh: "rotated".to_owned(),
            auth: "rotated".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("re-subscribing a held endpoint is always admitted");

    assert_eq!(
        fixture.endpoints_for(&fp).await.len(),
        MAX_SUBSCRIPTIONS_PER_DEVICE
    );
    assert_eq!(
        fixture.keys_for(&first).await,
        Some(("rotated".to_owned(), "rotated".to_owned())),
        "the keys must be the new ones, not the old"
    );
}

#[tokio::test]
async fn two_devices_hold_independent_caps() {
    let fixture = PushFixture::new("cap-independent").await;
    let other_fp = viewer_fp('b');
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &other_fp,
            &format!("{PUSH_ORIGIN}/other-device"),
        )
        .await;

    for index in 0..MAX_SUBSCRIPTIONS_PER_DEVICE {
        handle_push_subscribe(
            &fixture.core,
            &fixture.caller,
            subscribe_request(&format!("{PUSH_ORIGIN}/mine-{index}")),
        )
        .await
        .expect("within the cap");
    }
    let overflow = handle_push_subscribe(
        &fixture.core,
        &fixture.caller,
        subscribe_request(&format!("{PUSH_ORIGIN}/mine-overflow")),
    )
    .await
    .expect_err("this device is full");

    assert_eq!(overflow.code, ErrorCode::ResourceExhausted);
    assert_eq!(
        fixture.endpoints_for(&other_fp).await.len(),
        1,
        "another device's subscriptions are not this device's cap"
    );
}

#[tokio::test]
async fn two_concurrent_subscribes_for_one_device_cannot_both_land_past_the_cap() {
    let fixture = PushFixture::new("cap-race").await;
    let fp = fixture.fp();
    // Three held, so the fourth slot is the one two racers contend for.
    for index in 0..3 {
        handle_push_subscribe(
            &fixture.core,
            &fixture.caller,
            subscribe_request(&format!("{PUSH_ORIGIN}/held-{index}")),
        )
        .await
        .expect("within the cap");
    }

    // Two DISTINCT endpoints racing for the last slot. Exactly one may win: the
    // cap and the write are one statement, so there is no window between "how
    // many does this device hold" and "here is one more". A count-then-insert
    // lets both observe three and both insert, which is the defect this shape
    // exists to prevent.
    let (left, right) = tokio::join!(
        handle_push_subscribe(
            &fixture.core,
            &fixture.caller,
            subscribe_request(&format!("{PUSH_ORIGIN}/racer-left")),
        ),
        handle_push_subscribe(
            &fixture.core,
            &fixture.caller,
            subscribe_request(&format!("{PUSH_ORIGIN}/racer-right")),
        ),
    );
    let admitted = [left.is_ok(), right.is_ok()]
        .iter()
        .filter(|ok| **ok)
        .count();
    let refused = [left, right]
        .iter()
        .filter(|outcome| {
            outcome
                .as_ref()
                .err()
                .is_some_and(|error| error.code == ErrorCode::ResourceExhausted)
        })
        .count();

    assert_eq!(admitted, 1, "exactly one racer takes the last slot");
    assert_eq!(refused, 1, "the other racer is told the cap");
    let held = fixture.endpoints_for(&fp).await;
    assert_eq!(
        held.len(),
        MAX_SUBSCRIPTIONS_PER_DEVICE,
        "the device never exceeds the cap, however the two interleaved: {held:?}"
    );
}

#[tokio::test]
async fn a_concurrent_race_at_every_slot_still_lands_exactly_the_cap() {
    let fixture = PushFixture::new("cap-race-all").await;
    let fp = fixture.fp();

    // Eight racers, two slots. Whatever the interleaving, the cap is the answer.
    let requests: Vec<PushSubscribeRequest> = (0..8)
        .map(|index| subscribe_request(&format!("{PUSH_ORIGIN}/racer-{index}")))
        .collect();
    let mut admitted = 0;
    for chunk in requests.chunks(2) {
        let (left, right) = tokio::join!(
            handle_push_subscribe(&fixture.core, &fixture.caller, chunk[0].clone()),
            handle_push_subscribe(&fixture.core, &fixture.caller, chunk[1].clone()),
        );
        admitted += usize::from(left.is_ok()) + usize::from(right.is_ok());
    }

    assert_eq!(
        admitted, MAX_SUBSCRIPTIONS_PER_DEVICE,
        "four rounds of two racers fill exactly four slots"
    );
    assert_eq!(
        fixture.endpoints_for(&fp).await.len(),
        MAX_SUBSCRIPTIONS_PER_DEVICE
    );
}

#[tokio::test]
async fn subscribe_admits_a_well_formed_request_and_unsubscribe_removes_it() {
    let fixture = PushFixture::new("roundtrip").await;
    let fp = fixture.fp();
    let endpoint = format!("{PUSH_ORIGIN}/subscription-token");

    let subscribed = handle_push_subscribe(
        &fixture.core,
        &fixture.caller,
        PushSubscribeRequest {
            endpoint: endpoint.clone(),
            p256dh: "abc".to_owned(),
            auth: "def".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("a well-formed subscribe is admitted");
    assert!(subscribed.body.ok);
    assert_eq!(fixture.endpoints_for(&fp).await, vec![endpoint.clone()]);

    // A second subscribe of the same endpoint updates rather than duplicating.
    handle_push_subscribe(&fixture.core, &fixture.caller, subscribe_request(&endpoint))
        .await
        .expect("a repeat subscribe is admitted");
    assert_eq!(fixture.endpoints_for(&fp).await, vec![endpoint.clone()]);

    let unsubscribed = handle_push_unsubscribe(
        &fixture.core,
        &fixture.caller,
        PushUnsubscribeRequest {
            endpoint: endpoint.clone(),
            ..Default::default()
        },
    )
    .await
    .expect("a well-formed unsubscribe is admitted");
    assert!(unsubscribed.body.ok);
    assert!(fixture.endpoints_for(&fp).await.is_empty());
}

#[tokio::test]
async fn an_over_length_endpoint_is_refused_and_stores_nothing() {
    let fixture = PushFixture::new("endpoint-long").await;
    let fp = fixture.fp();
    let too_long = format!(
        "{PUSH_ORIGIN}/{}",
        "t".repeat(roost_coord::push::endpoint_policy::ENDPOINT_MAX_LENGTH)
    );

    let error = handle_push_subscribe(&fixture.core, &fixture.caller, subscribe_request(&too_long))
        .await
        .expect_err("an endpoint past the bound is refused");
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(
        fixture.endpoints_for(&fp).await.is_empty(),
        "a refused subscribe must not leave a row"
    );
}

#[tokio::test]
async fn a_malformed_key_is_refused_for_both_key_fields() {
    let fixture = PushFixture::new("key-malformed").await;
    let fp = fixture.fp();

    for (field, request) in [
        (
            "p256dh",
            PushSubscribeRequest {
                endpoint: format!("{PUSH_ORIGIN}/key-p256dh"),
                p256dh: "abc+def".to_owned(),
                auth: "def".to_owned(),
                ..Default::default()
            },
        ),
        (
            "auth",
            PushSubscribeRequest {
                endpoint: format!("{PUSH_ORIGIN}/key-auth"),
                p256dh: "abc".to_owned(),
                auth: "not base64url!".to_owned(),
                ..Default::default()
            },
        ),
        (
            "auth",
            PushSubscribeRequest {
                endpoint: format!("{PUSH_ORIGIN}/key-empty"),
                p256dh: "abc".to_owned(),
                auth: String::new(),
                ..Default::default()
            },
        ),
    ] {
        let error = handle_push_subscribe(&fixture.core, &fixture.caller, request)
            .await
            .expect_err(&format!("{field} must be refused"));
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{field}");
        assert!(
            error
                .message
                .as_deref()
                .unwrap_or_default()
                .contains(field.split(' ').next().unwrap_or(field)),
            "the refusal must name which key: {field} -> {error}"
        );
    }
    assert!(
        fixture.endpoints_for(&fp).await.is_empty(),
        "no malformed key may leave a row behind"
    );
}

#[tokio::test]
async fn an_endpoint_outside_the_operator_allowlist_is_refused() {
    let fixture = PushFixture::new("endpoint-allowlist").await;
    let fp = fixture.fp();

    for endpoint in [
        "https://push.example.attacker.invalid/subscription",
        "https://127.0.0.1/subscription",
        "https://push.example:8443/subscription",
        "https://user@push.example/subscription",
        "https://push.example/subscription#fragment",
        "http://push.example/subscription",
    ] {
        let error =
            handle_push_subscribe(&fixture.core, &fixture.caller, subscribe_request(endpoint))
                .await
                .expect_err("an undeclared origin is refused");
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{endpoint}");
    }
    assert!(fixture.endpoints_for(&fp).await.is_empty());
}

#[tokio::test]
async fn push_is_unavailable_rather_than_permissive_with_no_configured_origin() {
    let fixture =
        PushFixture::build("disabled", Vec::new(), push_fixture::production_generator()).await;
    let fp = fixture.fp();

    for error in [
        handle_push_subscribe(
            &fixture.core,
            &fixture.caller,
            subscribe_request(&format!("{PUSH_ORIGIN}/disabled")),
        )
        .await
        .expect_err("subscribe is refused"),
        handle_push_unsubscribe(
            &fixture.core,
            &fixture.caller,
            PushUnsubscribeRequest {
                endpoint: format!("{PUSH_ORIGIN}/disabled"),
                ..Default::default()
            },
        )
        .await
        .expect_err("unsubscribe is refused"),
    ] {
        assert_eq!(
            error.code,
            ErrorCode::FailedPrecondition,
            "a deployment with Push off must not invite a retry loop"
        );
    }
    assert!(fixture.endpoints_for(&fp).await.is_empty());
}
