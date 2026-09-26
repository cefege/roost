//! The four relay methods against a relay this dashboard owns: the rows they
//! write, the stream each mutation publishes, and the order the registry answers
//! in.
//!
//! The refusals are in `mcp_relays_refusals.rs` and the boundary is in
//! `mcp_relays_tenancy.rs`; this file is the happy path, and it is where the two
//! representations of a relay are pinned apart — the RPC echoes the caller's
//! config bytes while the stream carries the parsed object, and a port that
//! collapses them passes every other test in the suite.

// `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
// test is its own crate rather than a module of one, so the exemption has to be
// stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "mcp_relays_support/mod.rs"]
mod mcp_relays_support;

use mcp_relays_support::{McpFixture, create_request, delete_request, publish_request};
use roost_coord::sessions::mcp::{
    handle_mcp_create, handle_mcp_delete, handle_mcp_list, handle_mcp_publish,
};
use roost_proto as proto;
use roost_protocol::wire::{McpRelayDelta, McpStreamMessage};

#[tokio::test]
async fn create_list_publish_and_delete_stay_consistent_with_the_relay_stream() {
    let fixture = McpFixture::new("stream").await;
    let device = fixture.device();
    let config = r#"{"command":"bun","args":["run","mcp"]}"#;

    let (created_id, messages) = fixture
        .collect(async {
            let created = handle_mcp_create(
                &fixture.core,
                &device,
                create_request("Local tools", "stdio", config),
            )
            .await
            .expect("a relay is registered")
            .body;
            let relay = created.relay.as_option().expect("the created relay");
            assert_eq!(relay.label, "Local tools");
            assert_eq!(relay.kind, "stdio");
            assert_eq!(
                relay.config_json, config,
                "the RPC echoes the caller's bytes rather than a re-rendering"
            );
            let id = relay.id.clone();

            let listed = handle_mcp_list(&fixture.core, &device, proto::McpListRequest::default())
                .await
                .expect("the registry is listed")
                .body;
            assert_eq!(listed.relays.len(), 1);
            assert_eq!(listed.relays[0].id, id);
            assert!(
                listed.relays[0].created_at_ms > 0,
                "the registry answers with the instant the row was written"
            );

            let published = handle_mcp_publish(
                &fixture.core,
                &device,
                publish_request(&id, r#"{"method":"tools/list"}"#),
            )
            .await
            .expect("a payload is accepted")
            .body;
            assert!(
                published.ok,
                "acceptance is what the method answers, and nothing more"
            );

            let deleted = handle_mcp_delete(&fixture.core, &device, delete_request(&id))
                .await
                .expect("the relay is removed")
                .body;
            assert!(deleted.ok);
            id
        })
        .await;

    assert!(
        handle_mcp_list(&fixture.core, &device, proto::McpListRequest::default())
            .await
            .expect("the registry is listed")
            .body
            .relays
            .is_empty(),
        "the deleted relay is gone from the registry"
    );

    assert_eq!(messages.len(), 3, "one stream message per state transition");
    let McpStreamMessage::Delta(McpRelayDelta::Created { relay }) = &messages[0] else {
        panic!("the first message announces the relay");
    };
    assert_eq!(relay.id.as_str(), created_id);
    assert_eq!(relay.label, "Local tools");
    assert_eq!(relay.kind.as_str(), "stdio");
    assert_eq!(
        relay.config.get("command").and_then(|value| value.as_str()),
        Some("bun"),
        "the stream carries the PARSED config, which is not the RPC's raw text"
    );
    let McpStreamMessage::Event(event) = &messages[1] else {
        panic!("the second message is the published payload");
    };
    assert_eq!(event.relay_id.as_str(), created_id);
    assert_eq!(
        event.payload.get("method").and_then(|value| value.as_str()),
        Some("tools/list")
    );
    assert!(event.ts > 0, "the payload is stamped when it was accepted");
    let McpStreamMessage::Delta(McpRelayDelta::Deleted { id }) = &messages[2] else {
        panic!("the third message removes the relay");
    };
    assert_eq!(id.as_str(), created_id);
}

#[tokio::test]
async fn a_created_relay_persists_the_resolved_dashboard_id() {
    let fixture = McpFixture::new("scope").await;
    let created = handle_mcp_create(
        &fixture.core,
        &fixture.device(),
        create_request(
            "Scoped tools",
            "sse",
            r#"{"url":"http://127.0.0.1:9999/sse"}"#,
        ),
    )
    .await
    .expect("a relay is registered")
    .body;
    let id = created
        .relay
        .as_option()
        .expect("the created relay")
        .id
        .clone();

    assert_eq!(
        fixture.rows().await,
        vec![(id, fixture.dashboard_id.clone())],
        "the row is written under the dashboard boot resolved, not under a literal"
    );
}

#[tokio::test]
async fn the_registry_answers_in_the_order_it_declares() {
    let fixture = McpFixture::new("order").await;
    let device = fixture.device();
    // Two rows created inside the same millisecond, so the order cannot come
    // from the clock: it has to come from the statement. SQLite answers a bare
    // `SELECT` in rowid order, which is insertion order, and two ids minted in
    // sequence do not sort that way -- so this pins the `ORDER BY` instead of
    // restating whichever order happened to come out.
    let mut ids = Vec::new();
    for label in ["first", "second"] {
        let created =
            handle_mcp_create(&fixture.core, &device, create_request(label, "stdio", "{}"))
                .await
                .expect("a relay is registered")
                .body;
        ids.push(
            created
                .relay
                .as_option()
                .expect("the created relay")
                .id
                .clone(),
        );
    }

    let listed = handle_mcp_list(&fixture.core, &device, proto::McpListRequest::default())
        .await
        .expect("the registry is listed")
        .body;
    let answered: Vec<String> = listed.relays.iter().map(|relay| relay.id.clone()).collect();
    let mut by_id = ids.clone();
    by_id.sort();
    assert_eq!(
        answered, by_id,
        "with equal timestamps the declared order is the id order"
    );

    let asked_again = handle_mcp_list(&fixture.core, &device, proto::McpListRequest::default())
        .await
        .expect("the registry is listed")
        .body;
    assert_eq!(
        asked_again
            .relays
            .iter()
            .map(|relay| relay.id.clone())
            .collect::<Vec<_>>(),
        answered,
        "a second ask answers the same way"
    );
}
