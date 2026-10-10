//! Web-search provider credentials and settings through coordinator RPCs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;
mod db_support;

use agent_fixture::AgentFixture;
use connectrpc::ErrorCode;
use roost_coord::agent::rpc_accounts::{handle_agent_settings_get, handle_agent_settings_set};
use roost_coord::agent::rpc_auth::handle_agent_auth_set_api_key;
use roost_proto as proto;
use serde_json::Value;

#[tokio::test]
async fn web_search_api_key_allow_list_accepts_brave_and_rejects_unknown_providers() {
    let fixture = AgentFixture::new("web-search-api-key").await;
    handle_agent_auth_set_api_key(
        &fixture.core,
        &fixture.caller,
        proto::AgentAuthSetApiKeyRequest {
            provider: "brave".into(),
            api_key: "brave-test-key-0001".into(),
            ..Default::default()
        },
    )
    .await
    .expect("Brave API key accepted");

    let error = handle_agent_auth_set_api_key(
        &fixture.core,
        &fixture.caller,
        proto::AgentAuthSetApiKeyRequest {
            provider: "not-a-search-provider".into(),
            api_key: "test-key".into(),
            ..Default::default()
        },
    )
    .await
    .expect_err("unknown provider rejected");
    assert_eq!(error.code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn web_search_settings_round_trip_searxng_url_and_role_selector() {
    let fixture = AgentFixture::new("web-search-settings").await;
    handle_agent_settings_set(
        &fixture.core,
        &fixture.caller,
        proto::AgentSettingsSetRequest {
            settings_json:
                r#"{"model_roles":{"web":"web/brave"},"searxng_url":"https://search.example.test"}"#
                    .into(),
            ..Default::default()
        },
    )
    .await
    .expect("web search settings accepted");

    let response = handle_agent_settings_get(
        &fixture.core,
        &fixture.caller,
        proto::AgentSettingsGetRequest::default(),
    )
    .await
    .expect("settings returned");
    let settings: Value =
        serde_json::from_str(&response.body.settings_json).expect("valid settings JSON");
    assert_eq!(settings["model_roles"]["web"], "web/brave");
    assert_eq!(settings["searxng_url"], "https://search.example.test");
}
