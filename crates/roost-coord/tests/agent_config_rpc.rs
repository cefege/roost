//! Device-authorized OMP auto-launch persistence through the actual RPC handlers.
//! A switch change must survive a separate read and an ensuing disable.
//! Retired agent selections and custom commands have no executable API fields.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;
mod db_support;

use agent_fixture::AgentFixture;
use roost_coord::agents::rpc_status::{handle_agent_config_get, handle_agent_config_set};
use roost_proto as proto;

#[tokio::test]
async fn omp_auto_launch_changes_survive_reads_and_can_be_disabled() {
    let fixture = AgentFixture::new("rpc-config").await;
    for enabled in [true, false] {
        handle_agent_config_set(
            &fixture.core,
            &fixture.caller,
            proto::AgentConfigSetRequest {
                auto_launch: enabled,
                ..Default::default()
            },
        )
        .await
        .expect("store OMP auto-launch");
        let read_back = handle_agent_config_get(
            &fixture.core,
            &fixture.caller,
            proto::AgentConfigGetRequest::default(),
        )
        .await
        .expect("read persisted OMP auto-launch")
        .body;
        assert_eq!(read_back.auto_launch, enabled);
    }
}
