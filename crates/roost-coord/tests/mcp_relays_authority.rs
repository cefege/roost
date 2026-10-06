//! Two properties that are about the coordinator rather than about a relay: who
//! may touch the registry at all, and what a caller waits for when the store
//! cannot answer.
//!
//! The authority half is the port of v2's rule that a machine has no standing
//! over the relay registry. The deadline half is the only test here that costs
//! wall clock (~5 s), because the only honest way to make a store unresponsive
//! is to hold the coordinator's single connection and let the real timeout fire.

// `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
// test is its own crate rather than a module of one, so the exemption has to be
// stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
#[path = "mcp_relays_support/mod.rs"]
mod mcp_relays_support;

use std::time::Instant;

use connectrpc::ErrorCode;
use mcp_relays_support::{
    McpFixture, UNKNOWN_RELAY, create_request, delete_request, message_of, publish_request,
};
use roost_coord::sessions::mcp::{
    handle_mcp_create, handle_mcp_delete, handle_mcp_list, handle_mcp_publish,
};
use roost_proto as proto;

#[tokio::test]
async fn a_worker_principal_has_no_authority_over_the_relay_registry() {
    let fixture = McpFixture::new("worker").await;
    let worker = fixture.worker();

    let listed = handle_mcp_list(&fixture.core, &worker, proto::McpListRequest::default())
        .await
        .expect_err("a machine cannot read the registry");
    assert_eq!(listed.code, ErrorCode::Unauthenticated);

    let created = handle_mcp_create(
        &fixture.core,
        &worker,
        create_request("Worker tools", "stdio", "{}"),
    )
    .await
    .expect_err("a machine cannot register a relay");
    assert_eq!(created.code, ErrorCode::Unauthenticated);

    let published =
        handle_mcp_publish(&fixture.core, &worker, publish_request(UNKNOWN_RELAY, "{}"))
            .await
            .expect_err("a machine cannot publish a payload");
    assert_eq!(published.code, ErrorCode::Unauthenticated);

    let deleted = handle_mcp_delete(&fixture.core, &worker, delete_request(UNKNOWN_RELAY))
        .await
        .expect_err("a machine cannot remove a relay");
    assert_eq!(deleted.code, ErrorCode::Unauthenticated);
    assert!(fixture.rows().await.is_empty());
}

#[tokio::test]
async fn a_publish_the_store_cannot_answer_is_refused_inside_the_busy_timeout() {
    let fixture = McpFixture::new("deadline").await;
    // Holding every pooled connection is what an unresponsive store looks like
    // from a handler: the statement can never start.
    let held = db_support::hold_every_connection(fixture.database()).await;

    let started = Instant::now();
    let refused = handle_mcp_publish(
        &fixture.core,
        &fixture.device(),
        publish_request(UNKNOWN_RELAY, r#"{"method":"tools/list"}"#),
    )
    .await
    .expect_err("a store that cannot answer is refused, not waited on");
    let elapsed = started.elapsed();

    assert_eq!(
        refused.code,
        ErrorCode::Unavailable,
        "the call is retryable, so it is neither the caller's fault nor a statement failure"
    );
    assert!(
        message_of(&refused).contains("McpPublish"),
        "the refusal names the method that was waiting: {}",
        message_of(&refused)
    );
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "the caller was held for {elapsed:?}, which is not a bound"
    );
    drop(held);
}
