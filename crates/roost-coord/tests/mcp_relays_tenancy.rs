//! The tenancy boundary, which is what this domain is really about: the surface
//! is install-wide by construction -- no session column, no session field on the
//! proto, none on the stream frame -- so the boundary under test is the dashboard
//! the row belongs to.
//!
//! It is asserted with a row that EXISTS and is deliberately invisible, because
//! the property is not "unknown ids are refused" (v2 already had that) but "an id
//! this coordinator does not hold changes nothing here and reaches nobody's
//! stream". A row planted in another dashboard is the only way to tell those two
//! apart, which is why the fixture that plants one lives in the same binary.

// `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
// test is its own crate rather than a module of one, so the exemption has to be
// stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "mcp_relays_support/mod.rs"]
mod mcp_relays_support;

use std::sync::Arc;

use connectrpc::ErrorCode;
use mcp_relays_support::{
    FOREIGN_DASHBOARD, FOREIGN_RELAY, McpFixture, delete_request, message_of, plant_foreign_relay,
    publish_request,
};
use roost_coord::coord_core::CoordCore;
use roost_coord::services::CoordServices;
use roost_coord::sessions::mcp::{handle_mcp_delete, handle_mcp_list, handle_mcp_publish};
use roost_proto as proto;

#[tokio::test]
async fn a_relay_another_dashboard_holds_is_not_found_and_changes_nothing() {
    let fixture = McpFixture::new("foreign").await;
    plant_foreign_relay(&fixture).await;
    let device = fixture.device();

    let (published, published_messages) = fixture
        .collect(async {
            handle_mcp_publish(
                &fixture.core,
                &device,
                publish_request(FOREIGN_RELAY, r#"{"method":"tools/list"}"#),
            )
            .await
        })
        .await;
    let published = published.expect_err("a relay outside this dashboard is not publishable");
    assert_eq!(published.code, ErrorCode::NotFound);
    assert!(
        published_messages.is_empty(),
        "a call for a relay this coordinator does not hold must reach no stream"
    );

    let deleted = handle_mcp_delete(&fixture.core, &device, delete_request(FOREIGN_RELAY))
        .await
        .expect_err("a relay outside this dashboard is not deletable");
    assert_eq!(
        deleted.code, published.code,
        "an unknown id and a foreign id are one answer, so the registry cannot be probed"
    );
    assert_eq!(
        fixture.rows().await,
        vec![(FOREIGN_RELAY.to_owned(), FOREIGN_DASHBOARD.to_owned())],
        "the refused delete left the row exactly as it was"
    );

    let listed = handle_mcp_list(&fixture.core, &device, proto::McpListRequest::default())
        .await
        .expect("the registry is listed")
        .body;
    assert!(
        listed.relays.is_empty(),
        "a registry answer names only what this dashboard holds"
    );
}

#[tokio::test]
async fn a_coordinator_with_no_tenancy_refuses_rather_than_reading_every_row() {
    let fixture = McpFixture::new("unbooted").await;
    // `CoordServices::new` is what a process built without the boot step gets,
    // and the answer has to name the wiring fault rather than fall back to "all
    // rows", which is a different registry rather than a refusal.
    let unbooted = CoordCore::new(Arc::new(CoordServices::new(fixture.database().clone())));
    let refused = handle_mcp_list(
        &unbooted,
        &fixture.device(),
        proto::McpListRequest::default(),
    )
    .await
    .expect_err("an unbooted coordinator has no registry to read");
    assert_eq!(refused.code, ErrorCode::Internal);
    assert_eq!(message_of(&refused), "coordinator booted without tenant");
}
