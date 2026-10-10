//! The Web Push a new pairing request sends: every subscribed device on an
//! allowed origin hears that a browser is waiting, under a per-request
//! deduplication token, and nothing is sent without an allowlist.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod push_fixture;

use push_fixture::{PUSH_ORIGIN, PushFixture};
use roost_coord::push::pair_request::fire_pair_request_push;
use serde_json::Value;

const REQUESTER: &str = "Chrome on macOS · Berlin";

#[tokio::test]
async fn a_pairing_request_is_pushed_to_every_allowed_subscription() {
    let fixture = PushFixture::new("pair-push").await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &fixture.fp(),
            &format!("{PUSH_ORIGIN}/pair"),
        )
        .await;
    let transport = push_fixture::FakeTransport::accepting();
    let ephemeral_id = "e".repeat(32);

    fire_pair_request_push(
        fixture.database().pool(),
        &ephemeral_id,
        REQUESTER,
        &[PUSH_ORIGIN.to_owned()],
        transport.as_ref(),
    )
    .await;

    let deliveries = transport.deliveries();
    assert_eq!(deliveries.len(), 1);
    let payload: Value = serde_json::from_str(&deliveries[0].body).expect("the payload is JSON");
    assert_eq!(payload["kind"], "pair_request");
    assert_eq!(payload["ephemeralId"], ephemeral_id.as_str());
    assert_eq!(payload["title"], "A browser wants to pair");
    assert!(
        payload["body"]
            .as_str()
            .is_some_and(|body| body.contains(REQUESTER)),
        "{payload}"
    );
    assert_eq!(deliveries[0].topic.as_deref().map(str::len), Some(32));
}

#[tokio::test]
async fn no_allowed_origin_sends_nothing() {
    let fixture = PushFixture::new("pair-push-none").await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &fixture.fp(),
            &format!("{PUSH_ORIGIN}/pair"),
        )
        .await;
    let transport = push_fixture::FakeTransport::accepting();

    fire_pair_request_push(
        fixture.database().pool(),
        &"e".repeat(32),
        REQUESTER,
        &[],
        transport.as_ref(),
    )
    .await;

    assert!(transport.deliveries().is_empty());
}
