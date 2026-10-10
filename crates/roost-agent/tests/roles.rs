//! Model selectors and role resolution: syntax, fallbacks, and the judge
//! chain's rule that a native judge is never replaced by a prompted one.

#[path = "support/mod.rs"]
mod support;

use roost_agent::roles::{SelectorCandidate, judge_candidates, parse_selector, resolve_role};
use roost_agent::{AgentSettings, Role};
use support::ScriptedLlm;

#[test]
fn selectors_parse_models_roles_thinking_and_lists() {
    assert_eq!(
        parse_selector("anthropic/claude-opus-5:high, @smol").unwrap(),
        vec![
            SelectorCandidate::Model {
                provider: "anthropic".into(),
                id: "claude-opus-5".into(),
                thinking: Some("high".into())
            },
            SelectorCandidate::Role(Role::Smol),
        ]
    );
    assert_eq!(
        parse_selector("openrouter/respan/span-01-lite:free").unwrap(),
        vec![SelectorCandidate::Model {
            provider: "openrouter".into(),
            id: "respan/span-01-lite:free".into(),
            thinking: None
        }]
    );
    assert!(parse_selector("@nope").is_err());
    assert!(parse_selector("no-slash").is_err());
    assert!(parse_selector("a/b,,c/d").is_err());
}

#[tokio::test]
async fn unconfigured_smol_falls_back_to_the_default_role() {
    let llm = ScriptedLlm::new(&["anthropic"]);
    let mut settings = AgentSettings::default();
    settings
        .model_roles
        .insert(Role::Default, "anthropic/claude-haiku-4-5".into());
    let smol = resolve_role(llm.as_ref(), &settings, Role::Smol)
        .await
        .unwrap();
    assert_eq!(smol.info.id, "claude-haiku-4-5");
    settings
        .model_roles
        .insert(Role::Advisor, "openai-codex/gpt-6-sol".into());
    assert!(
        resolve_role(llm.as_ref(), &settings, Role::Advisor)
            .await
            .is_none(),
        "an unavailable configured advisor does not fall back"
    );
}

#[tokio::test]
async fn the_judge_chain_keeps_only_native_candidates_after_the_first_native_one() {
    let llm = ScriptedLlm::new(&["anthropic", "typesafe"]);
    let mut settings = AgentSettings::default();
    settings.model_roles.insert(
        Role::Judge,
        "anthropic/claude-haiku-4-5, typesafe/jev-latest, @default".into(),
    );
    let chain = judge_candidates(llm.as_ref(), &settings, None).await;
    let ids: Vec<&str> = chain.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, vec!["claude-haiku-4-5", "jev-latest"]);

    let chat_only = ScriptedLlm::new(&["anthropic"]);
    let chain = judge_candidates(chat_only.as_ref(), &AgentSettings::default(), None).await;
    assert!(!chain.is_empty() && chain.iter().all(|model| model.provider == "anthropic"));
}
