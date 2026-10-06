//! What the relay methods refuse, and the order they refuse in: a config the
//! coordinator cannot describe, a payload it cannot represent, an id that is not
//! a uuid, a kind the wire does not define, and an empty label.
//!
//! Every one of these must be refused BEFORE anything persists or is published.
//! That ordering is the property, not the status code: a row that persisted
//! behind a refused request is the `docs/FAILURE-INDEX.md` entry "JSON.parse
//! inside a bus publish, after the commit", where the RPC fails, the row stays,
//! and the SPA keeps showing the prior state until a manual refresh.

// `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
// test is its own crate rather than a module of one, so the exemption has to be
// stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
#[path = "mcp_relays_support/mod.rs"]
mod mcp_relays_support;

use connectrpc::ErrorCode;
use mcp_relays_support::{
    McpFixture, UNKNOWN_RELAY, create_request, delete_request, message_of, publish_request,
};
use roost_coord::sessions::mcp::{
    handle_mcp_create, handle_mcp_delete, handle_mcp_list, handle_mcp_publish,
};
use roost_proto as proto;

#[tokio::test]
async fn a_config_that_is_not_a_json_object_is_refused_before_any_row_persists() {
    let fixture = McpFixture::new("config").await;
    let (refused, messages) = fixture
        .collect(async {
            handle_mcp_create(
                &fixture.core,
                &fixture.device(),
                create_request("Broken", "stdio", "[1,2,3]"),
            )
            .await
            .expect_err("an array is not a relay config")
        })
        .await;

    assert_eq!(refused.code, ErrorCode::InvalidArgument);
    assert!(
        message_of(&refused).contains("configJson"),
        "the refusal names the field: {}",
        message_of(&refused)
    );
    assert!(
        fixture.rows().await.is_empty(),
        "a row the announcement cannot describe must not persist"
    );
    assert!(
        messages.is_empty(),
        "nothing was announced for a relay that does not exist"
    );
}

#[tokio::test]
async fn an_unrepresentable_payload_is_refused_before_the_relay_is_resolved() {
    let fixture = McpFixture::new("precedence").await;
    // The relay does not exist either, so the refusal identifies which of the
    // two was checked first: a payload the coordinator cannot represent is not
    // worth resolving a relay for.
    let refused = handle_mcp_publish(
        &fixture.core,
        &fixture.device(),
        publish_request(UNKNOWN_RELAY, "{not json"),
    )
    .await
    .expect_err("a payload that is not JSON is refused");
    assert_eq!(refused.code, ErrorCode::InvalidArgument);
    assert!(
        message_of(&refused).contains("payloadJson"),
        "the unparseable payload is named, not the missing relay: {}",
        message_of(&refused)
    );
}

#[tokio::test]
async fn a_malformed_relay_id_is_refused_before_it_reaches_the_store() {
    let fixture = McpFixture::new("ids").await;
    let deleted = handle_mcp_delete(
        &fixture.core,
        &fixture.device(),
        delete_request("not-a-relay"),
    )
    .await
    .expect_err("a relay id that is not a uuid is refused");
    assert_eq!(deleted.code, ErrorCode::InvalidArgument);
    assert!(
        message_of(&deleted).contains("relay id"),
        "the refusal names the field: {}",
        message_of(&deleted)
    );

    let published = handle_mcp_publish(&fixture.core, &fixture.device(), publish_request("", "{}"))
        .await
        .expect_err("an empty relay id is refused");
    assert_eq!(published.code, ErrorCode::InvalidArgument);
    assert!(fixture.rows().await.is_empty());
}

#[tokio::test]
async fn an_unknown_relay_kind_and_an_empty_label_are_refused() {
    let fixture = McpFixture::new("kind").await;
    let empty_label = handle_mcp_create(
        &fixture.core,
        &fixture.device(),
        create_request("", "stdio", "{}"),
    )
    .await
    .expect_err("a relay with no label is refused");
    assert_eq!(empty_label.code, ErrorCode::InvalidArgument);
    assert_eq!(message_of(&empty_label), "label is required");

    let bad_kind = handle_mcp_create(
        &fixture.core,
        &fixture.device(),
        create_request("Carrier pigeon", "smoke-signal", "{}"),
    )
    .await
    .expect_err("a kind the wire does not define is refused");
    assert_eq!(bad_kind.code, ErrorCode::InvalidArgument);
    assert!(
        message_of(&bad_kind).contains("smoke-signal"),
        "the refusal quotes what arrived: {}",
        message_of(&bad_kind)
    );
    assert!(fixture.rows().await.is_empty());
}

#[tokio::test]
async fn a_relay_this_coordinator_cannot_see_is_refused_by_every_method_that_names_one() {
    let fixture = McpFixture::new("unseen").await;
    // The tenancy case is `mcp_relays_tenancy.rs`; this is the narrow half that
    // a port can get wrong on its own — naming an id that resolves to nothing
    // must be `NotFound` on the write paths and absent from the read, rather
    // than an empty success that a caller would read as "it was there".
    let device = fixture.device();
    let (published, messages) = fixture
        .collect(async {
            handle_mcp_publish(&fixture.core, &device, publish_request(UNKNOWN_RELAY, "{}")).await
        })
        .await;
    assert_eq!(
        published
            .expect_err("an unknown relay cannot be published to")
            .code,
        ErrorCode::NotFound
    );
    assert!(
        messages.is_empty(),
        "no stream message for a relay that is not there"
    );

    assert_eq!(
        handle_mcp_delete(&fixture.core, &device, delete_request(UNKNOWN_RELAY))
            .await
            .expect_err("an unknown relay cannot be deleted")
            .code,
        ErrorCode::NotFound
    );
    assert!(
        handle_mcp_list(&fixture.core, &device, proto::McpListRequest::default())
            .await
            .expect("the registry is listed")
            .body
            .relays
            .is_empty()
    );
}
