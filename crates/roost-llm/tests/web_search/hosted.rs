use super::*;
use std::collections::BTreeMap;

use roost_llm::{
    ModelInfo, ResolvedAuth, WireApi,
    web_search::{ParsedQuery, hosted_search},
};
use serde_json::Value;

fn model(provider: &str, api: WireApi, base_url: &str) -> ModelInfo {
    ModelInfo {
        provider: provider.into(),
        id: "fixture-model".into(),
        name: "fixture".into(),
        api,
        base_url: base_url.into(),
        reasoning: false,
        images: false,
        cost: Default::default(),
        context_window: 4096,
        max_tokens: 4096,
        thinking_level_map: BTreeMap::new(),
        kind: roost_llm::ModelKind::Chat,
        compat: Value::Null,
        intelligence: None,
        tokens_per_second: None,
        runner_kind: String::new(),
        priority: None,
        pricing_status: None,
    }
}

#[tokio::test]
async fn hosted_anthropic_codex_and_openrouter_responses_keep_citations() {
    let fixtures = [
        (
            include_str!("../fixtures/anthropic-web-search.json"),
            "text/event-stream",
            "anthropic",
            WireApi::AnthropicMessages,
        ),
        (
            include_str!("../fixtures/codex-web-search.sse"),
            "text/event-stream",
            "openai-codex",
            WireApi::OpenAiCodexResponses,
        ),
        (
            include_str!("../fixtures/openrouter-web-search.json"),
            "application/json",
            "openrouter",
            WireApi::OpenAiCompletions,
        ),
    ];
    for (fixture, content_type, provider, api) in fixtures {
        let (base, requests) = server(fixture, content_type).await;
        let model = model(provider, api, &base);
        let result = hosted_search(
            &reqwest::Client::new(),
            &Endpoints::default(),
            &model,
            ResolvedAuth::ApiKey {
                key: "test-key".into(),
            },
            &query("fixture"),
            &ParsedQuery::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(!result.sources.is_empty());
        assert!(result.answer.is_some());
        let request = requests.lock().await;
        let sent = request.first().unwrap();
        let body: Value = serde_json::from_str(&sent.body).unwrap();
        match provider {
            "anthropic" => {
                assert_eq!(body["tools"][0]["type"], "web_search_20250305");
                assert_eq!(sent.headers.get("x-api-key").unwrap(), "test-key");
            }
            "openai-codex" => {
                assert_eq!(body["tool_choice"]["type"], "web_search");
                assert_eq!(
                    sent.headers.get("openai-beta").unwrap(),
                    "responses=experimental"
                );
            }
            _ => assert_eq!(body["plugins"][0]["id"], "web"),
        }
    }
}

#[tokio::test]
async fn hosted_answer_without_sources_is_rejected() {
    let (base, _) = server(
        r#"{"content":[{"type":"text","text":"unsupported answer"}]}"#,
        "application/json",
    )
    .await;
    let result = hosted_search(
        &reqwest::Client::new(),
        &Endpoints::default(),
        &model("anthropic", WireApi::AnthropicMessages, &base),
        ResolvedAuth::ApiKey {
            key: "test-key".into(),
        },
        &query("fixture"),
        &ParsedQuery::default(),
        CancellationToken::new(),
    )
    .await;
    assert!(result.is_err());
}
