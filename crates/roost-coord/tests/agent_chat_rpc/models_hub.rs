//! What the models hub reads over the existing RPCs: recently used models,
//! per-model facts, and presets carried through settings.

use std::time::Duration;

use connectrpc::ErrorCode;
use roost_coord::agent::rpc_accounts::{handle_agent_settings_get, handle_agent_settings_set};
use roost_coord::agent::rpc_chat::{handle_agent_chat_configure, handle_agent_models_list};
use roost_proto as proto;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelRef, ModelsCatalog};

use super::{MODEL, Setup, add_key, create, setup};

async fn catalog(setup: &Setup) -> ModelsCatalog {
    let response = handle_agent_models_list(
        &setup.fixture.core,
        &setup.fixture.caller,
        proto::AgentModelsListRequest::default(),
    )
    .await
    .expect("catalog listed");
    serde_json::from_str(&response.body.catalog_json).unwrap()
}

fn anthropic(model_id: &str) -> ModelRef {
    ModelRef {
        provider: "anthropic".into(),
        model_id: model_id.into(),
    }
}

#[tokio::test]
async fn the_catalog_lists_recent_models_and_model_facts() {
    let setup = setup("agent-models-hub-recent").await;
    add_key(&setup, "sk-ant-models-hub-key-0001").await;
    create(&setup).await;
    assert_eq!(catalog(&setup).await.recent_models, [anthropic(MODEL)]);

    let second = create(&setup).await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    handle_agent_chat_configure(
        &setup.fixture.core,
        &setup.fixture.caller,
        proto::AgentChatConfigureRequest {
            conversation_id: second,
            model_provider: Some("anthropic".into()),
            model_id: Some("claude-opus-5".into()),
            ..Default::default()
        },
    )
    .await
    .expect("model configured");
    let listed = catalog(&setup).await;
    assert_eq!(
        listed.recent_models,
        [anthropic("claude-opus-5"), anthropic(MODEL)]
    );

    let sonnet = listed
        .models
        .iter()
        .find(|model| model.provider == "anthropic" && model.model_id == "claude-sonnet-5")
        .expect("sonnet listed");
    assert_eq!(sonnet.context_window, 1_000_000);
    assert_eq!(sonnet.input_cost_micros, 2_000_000);
    assert_eq!(sonnet.output_cost_micros, 10_000_000);
    assert!(sonnet.images);
    assert!(!sonnet.thinking_levels.is_empty());
}

#[tokio::test]
async fn settings_carry_presets_and_refuse_invalid_names() {
    let setup = setup("agent-models-hub-presets").await;
    let set = |json: &'static str| {
        handle_agent_settings_set(
            &setup.fixture.core,
            &setup.fixture.caller,
            proto::AgentSettingsSetRequest {
                settings_json: json.into(),
                ..Default::default()
            },
        )
    };
    set(r#"{"model_roles":{"default":"anthropic/claude-sonnet-5"},"model_presets":{"fast":{"default":"anthropic/claude-haiku-4-5"}}}"#)
        .await
        .expect("settings saved");
    let response = handle_agent_settings_get(
        &setup.fixture.core,
        &setup.fixture.caller,
        proto::AgentSettingsGetRequest::default(),
    )
    .await
    .expect("settings read");
    let view: AgentSettingsView = serde_json::from_str(&response.body.settings_json).unwrap();
    assert_eq!(
        view.model_presets["fast"]["default"],
        "anthropic/claude-haiku-4-5"
    );
    let error = set(r#"{"model_presets":{"1bad":{"default":"anthropic/claude-haiku-4-5"}}}"#)
        .await
        .expect_err("an invalid preset name was accepted");
    assert_eq!(error.code, ErrorCode::InvalidArgument);
}
