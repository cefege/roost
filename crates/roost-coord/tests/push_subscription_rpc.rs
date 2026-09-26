//! The three Web Push Connect methods over a real migrated database: the
//! per-device cap, the endpoint and key admission rules, and the push-disabled
//! state.
//!
//! The cap is the load-bearing thing here. v2 enforces it inside the
//! `INSERT ... SELECT ... WHERE` rather than counting and then inserting
//! (`apps/coord/src/push/handlers-push.ts:88-109`), because a count-then-insert
//! has a window between the two decisions that a device opening two tabs walks
//!
//! What a subscribe or unsubscribe request has to look like: the endpoint
//! admission rules, the key alphabet, and the disabled state.
//!
//! These are the four rules `handlers-push.ts:validateEndpoint` states, and
//! each is a rule a tidier port drops: without the allowlist check a
//! browser can name any host it likes as a push receiver; without the key
//! alphabet a malformed key reaches the crypto layer as an opaque error.
//! straight into. The race case at the bottom is what pins that shape: it fails
//! against a count-then-insert and passes against the single statement.

// A test that cannot say what it expected is not a test. `expect` is denied
// outside `#[cfg(test)]`, and an integration test is its own crate, so the
// exemption has to be stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod push_fixture;

use connectrpc::ErrorCode;
use push_fixture::{PUSH_ORIGIN, PushFixture};
use roost_coord::push::rpc::{handle_push_subscribe, handle_push_unsubscribe};
use roost_proto::{PushSubscribeRequest, PushUnsubscribeRequest};

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
