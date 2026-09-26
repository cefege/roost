//! The three Web Push Connect methods over a real migrated database: the
//! per-device cap, the endpoint and key admission rules, and the push-disabled
//! state.
//!
//! The cap is the load-bearing thing here. v2 enforces it inside the
//! `INSERT ... SELECT ... WHERE` rather than counting and then inserting
//! (`apps/coord/src/push/handlers-push.ts:88-109`), because a count-then-insert
//! has a window between the two decisions that a device opening two tabs walks
//!
//! The per-device subscription cap, and the race that keeps it honest.
//!
//! v2 enforces the cap inside the `INSERT ... SELECT ... WHERE` rather than
//! counting and then inserting (`apps/coord/src/push/handlers-push.ts:88-109`),
//! because a count-then-insert has a window between the two decisions that a
//! device opening two tabs walks straight into. The race cases at the bottom
//! are what pin that shape: they fail against a count-then-insert.

mod push_fixture;

use connectrpc::ErrorCode;
use push_fixture::{PUSH_ORIGIN, PushFixture, viewer_fp};
use roost_coord::push::rpc::handle_push_subscribe;
use roost_coord::push::subscription_store::MAX_SUBSCRIPTIONS_PER_DEVICE;
use roost_proto::PushSubscribeRequest;

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
    // The endpoint the test will later re-subscribe MUST be one of the four it
    // fills the cap with. Subscribing a fifth, unheld endpoint is a different
    // test -- and the EXISTS arm would not apply to it, so the cap would
    // correctly refuse and the assertion below would be about the wrong thing.
    let first = format!("{PUSH_ORIGIN}/subscription-0");
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
    // The other device must EXIST before it can hold a subscription:
    // `push_subscriptions.viewer_fp` references `authorized_keys`, so seeding a
    // row for a fingerprint with no key row is refused by the foreign key --
    // which is the schema working, not the test's intent.
    let other_fp = viewer_fp('b');
    fixture.seed_device(&fixture.account_id, &other_fp).await;
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
