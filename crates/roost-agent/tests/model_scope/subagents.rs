//! Subagent role selection preserves an inherited chat pin until overridden.
//! The model-scope suite exercises real runtime requests against its scripted provider.
//! Shared conversation fixtures and model identifiers stay in the parent module.

use super::support::{ANY_SESSION, tool_reply};
use super::{AgentSettings, HAIKU, Harness, OPUS, Role, SONNET, model_ids, pin, text_reply};
#[tokio::test]
async fn an_unset_subagent_role_keeps_the_pin_it_inherited_until_its_role_is_configured() {
    let harness = Harness::new();
    for (agent, role, role_configured, expected) in [
        ("scout", Role::Smol, false, SONNET),
        ("scout", Role::Smol, true, HAIKU),
        ("reviewer", Role::Slow, false, SONNET),
        ("reviewer", Role::Slow, true, HAIKU),
    ] {
        let mut settings = AgentSettings::default();
        settings
            .model_roles
            .insert(Role::Default, format!("anthropic/{OPUS}"));
        settings
            .model_roles
            .extend(role_configured.then(|| (role, format!("anthropic/{HAIKU}"))));
        harness.runtime.set_settings(settings).await.unwrap();
        let parent = harness.conversation().await;
        harness
            .runtime
            .set_model(&parent, pin(SONNET))
            .await
            .unwrap();
        let arguments = serde_json::json!({
            "context": "model role audit",
            "tasks": [{"name": "Audit", "agent": agent, "task": "Report your result using yield."}],
        })
        .to_string();
        harness
            .llm
            .script(&parent, tool_reply(&[("delegate", "task", &arguments)]));
        harness
            .llm
            .script(&parent, text_reply("delegation complete"));
        harness.llm.script(
            ANY_SESSION,
            tool_reply(&[("report", "yield", r#"{"result":"role audit complete"}"#)]),
        );
        harness.submit(&parent, "delegate the role audit").await;
        harness.settle(&parent).await;
        let child = harness
            .runtime
            .conversations()
            .await
            .unwrap()
            .into_iter()
            .find(|conversation| conversation.parent_id.as_deref() == Some(parent.as_str()))
            .unwrap()
            .id;
        assert_eq!(
            model_ids(&harness, &child).last().map(String::as_str),
            Some(expected),
            "{agent} with its role configured: {role_configured}"
        );
    }
}
