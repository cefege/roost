//! `/modelpreset`: presets snapshot the all-folders roles, restore them on
//! switch, and settings refuse malformed presets.

#[path = "support/mod.rs"]
mod support;

use std::collections::BTreeMap;

use roost_agent::{AgentSettings, Role};
use roost_protocol::wire::agent_chat::{TranscriptItem, presets_with_builtins};
use support::Harness;

async fn set_default(harness: &Harness, chain: &str) {
    let mut settings = harness.runtime.settings().await.unwrap();
    settings.model_roles.insert(Role::Default, chain.into());
    harness.runtime.set_settings(settings).await.unwrap();
}

#[tokio::test]
async fn presets_save_switch_list_and_delete_the_global_roles() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    assert!(
        harness
            .runtime
            .submit(&id, "/modelpreset save fast".into())
            .await
            .is_err()
    );
    set_default(&harness, "anthropic/claude-sonnet-5").await;
    harness.submit(&id, "/modelpreset save fast").await;
    let saved = harness.runtime.settings().await.unwrap().model_presets;
    assert_eq!(
        saved.get("fast"),
        Some(&BTreeMap::from([(
            "default".to_owned(),
            "anthropic/claude-sonnet-5".to_owned()
        )]))
    );
    set_default(&harness, "anthropic/claude-opus-5").await;
    harness.submit(&id, "/modelpreset switch fast").await;
    let settings = harness.runtime.settings().await.unwrap();
    assert_eq!(
        settings.model_roles.get(&Role::Default).map(String::as_str),
        Some("anthropic/claude-sonnet-5")
    );
    harness.submit(&id, "/modelpreset").await;
    let transcript = harness.settle(&id).await;
    assert!(transcript.items.iter().any(|item| matches!(item,
        TranscriptItem::Notice { body, .. } if body.contains("fast (active)"))));
    harness.submit(&id, "/modelpreset delete fast").await;
    assert!(
        harness
            .runtime
            .settings()
            .await
            .unwrap()
            .model_presets
            .is_empty()
    );
    assert!(
        harness
            .runtime
            .submit(&id, "/modelpreset save 1bad".into())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn settings_refuse_a_preset_with_an_unknown_role() {
    let harness = Harness::new();
    let mut settings = AgentSettings::default();
    settings.model_presets.insert(
        "fast".into(),
        BTreeMap::from([("bogus".to_owned(), "anthropic/claude-sonnet-5".to_owned())]),
    );
    assert!(harness.runtime.set_settings(settings).await.is_err());
}

#[tokio::test]
async fn starter_presets_switch_unsaved_and_a_saved_copy_resets_on_delete() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    // Every starter is a preset the settings accept as saved.
    let settings = AgentSettings {
        model_presets: presets_with_builtins(&BTreeMap::new()),
        ..AgentSettings::default()
    };
    harness.runtime.set_settings(settings).await.unwrap();
    harness
        .runtime
        .set_settings(AgentSettings::default())
        .await
        .unwrap();

    harness.submit(&id, "/modelpreset switch OpenAI").await;
    let roles = harness.runtime.settings().await.unwrap().model_roles;
    assert_eq!(
        roles.get(&Role::Default).map(String::as_str),
        Some("openai-codex/gpt-6.1-sol")
    );

    set_default(&harness, "anthropic/claude-sonnet-5").await;
    harness.submit(&id, "/modelpreset save Anthropic").await;
    harness.submit(&id, "/modelpreset switch Anthropic").await;
    let roles = harness.runtime.settings().await.unwrap().model_roles;
    assert_eq!(
        roles.get(&Role::Default).map(String::as_str),
        Some("anthropic/claude-sonnet-5")
    );
    harness.submit(&id, "/modelpreset delete Anthropic").await;
    harness.submit(&id, "/modelpreset switch Anthropic").await;
    let roles = harness.runtime.settings().await.unwrap().model_roles;
    assert_eq!(
        roles.get(&Role::Default).map(String::as_str),
        Some("anthropic/claude-opus-5-5")
    );
}
