//! The default-agent (launch-button) configuration RPCs: what a fresh install
//! answers, what a device stores, and the one value the server owns -- the
//! fallback that keeps a blank selection from rendering a launch button with no
//! command behind it.
//!
//! The catalog of agents belongs to the SPA, so the coordinator stores raw
//! strings and refuses to validate an id it may not know yet.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;

use agent_fixture::AgentFixture;
use roost_coord::agents::rpc_status::{handle_agent_config_get, handle_agent_config_set};
use roost_proto as proto;

#[tokio::test]
async fn the_default_agent_is_shared_and_a_blank_selection_falls_back() {
    let fixture = AgentFixture::new("rpc-config").await;
    let fresh = handle_agent_config_get(
        &fixture.core,
        &fixture.caller,
        proto::AgentConfigGetRequest::default(),
    )
    .await
    .expect("a fresh install's configuration")
    .body;
    assert_eq!(fresh.selected, "omp");
    assert_eq!(fresh.custom_command, "");
    assert!(!fresh.auto_launch);

    let stored = handle_agent_config_set(
        &fixture.core,
        &fixture.caller,
        proto::AgentConfigSetRequest {
            selected: "custom".to_owned(),
            custom_command: "aider --model gpt".to_owned(),
            auto_launch: true,
            ..Default::default()
        },
    )
    .await
    .expect("a stored configuration")
    .body;
    assert_eq!(stored.selected, "custom");
    assert_eq!(stored.custom_command, "aider --model gpt");
    assert!(stored.auto_launch);

    let read_back = handle_agent_config_get(
        &fixture.core,
        &fixture.caller,
        proto::AgentConfigGetRequest::default(),
    )
    .await
    .expect("the stored configuration")
    .body;
    assert_eq!(read_back.selected, "custom");

    let blank = handle_agent_config_set(
        &fixture.core,
        &fixture.caller,
        proto::AgentConfigSetRequest {
            selected: "   ".to_owned(),
            custom_command: String::new(),
            auto_launch: false,
            ..Default::default()
        },
    )
    .await
    .expect("a blank selection")
    .body;
    assert_eq!(
        blank.selected, "omp",
        "a blank agent id would render a launch button with no command"
    );
    assert_eq!(blank.custom_command, "", "an empty command clears it");
}
