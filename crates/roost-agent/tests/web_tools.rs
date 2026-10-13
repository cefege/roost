//! Fetch and provider-chain search tool exposure and result delivery.

#[path = "support/mod.rs"]
mod support;

use std::collections::BTreeMap;

use roost_agent::records::AgentSettings;
use roost_agent::{FetchedPage, Role};
use roost_llm::web_search::{SearchResponse, SearchSource};
use support::{Harness, text_reply, tool_reply};

#[tokio::test]
async fn fetched_page_is_available_to_the_next_model_request() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.page(
        "https://example.com/docs",
        FetchedPage {
            final_url: "https://example.com/docs".into(),
            content_type: Some("text/html".into()),
            text: "The documented limit is 42.".into(),
        },
    );
    harness.llm.script(
        &id,
        tool_reply(&[("fetch1", "fetch", r#"{"url":"https://example.com/docs"}"#)]),
    );
    harness.llm.script(&id, text_reply("The limit is 42."));
    harness.submit(&id, "Find the documented limit").await;
    harness.settle(&id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 2);
    assert!(
        tool_results(&requests[1])
            .iter()
            .any(|text| text.contains("The documented limit is 42"))
    );
}

#[tokio::test]
async fn web_search_is_always_offered_even_without_a_provider_credential() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(&id, text_reply("No search credential."));
    harness.submit(&id, "Answer without search").await;
    harness.settle(&id).await;
    assert!(
        harness.llm.requests_for(&id)[0]
            .tools
            .iter()
            .any(|tool| tool.name == "web_search")
    );
}

#[tokio::test]
async fn failed_engine_falls_through_in_role_chain_order() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    configure_chain(&harness, "web/exa, web/duckduckgo").await;
    harness
        .llm
        .providers
        .lock()
        .unwrap()
        .extend(["exa".into(), "duckduckgo".into()]);
    harness
        .llm
        .engine("exa", Err(roost_llm::LlmError::Network("offline".into())));
    harness
        .llm
        .engine("duckduckgo", Ok(response("duckduckgo", "Found it")));
    harness.llm.script(
        &id,
        tool_reply(&[(
            "search1",
            "web_search",
            r#"{"query":"current roost release"}"#,
        )]),
    );
    harness
        .llm
        .script(&id, text_reply("The release is current."));
    harness.submit(&id, "Search release details").await;
    harness.settle(&id).await;

    assert_eq!(
        *harness.llm.search_calls.lock().unwrap(),
        vec!["exa", "duckduckgo"]
    );
    assert!(
        tool_results(&harness.llm.requests_for(&id)[1])
            .iter()
            .any(|text| text.contains("Found it"))
    );
}

#[tokio::test]
async fn hosted_candidate_searches_with_the_conversation_model() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    configure_chain(&harness, "web/hosted").await;
    harness.llm.hosted(Ok(response("hosted", "Hosted result")));
    harness.llm.script(
        &id,
        tool_reply(&[(
            "search1",
            "web_search",
            r#"{"query":"current roost release"}"#,
        )]),
    );
    harness
        .llm
        .script(&id, text_reply("The hosted search found it."));
    harness.submit(&id, "Search release details").await;
    harness.settle(&id).await;

    assert_eq!(
        *harness.llm.hosted_models.lock().unwrap(),
        vec!["anthropic/claude-sonnet-5"]
    );
}

#[tokio::test]
async fn searxng_uses_the_saved_base_url() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .runtime
        .set_settings(AgentSettings {
            model_roles: BTreeMap::from([(Role::Web, "web/searxng".into())]),
            searxng_url: Some("https://search.example".into()),
            ..AgentSettings::default()
        })
        .await
        .unwrap();
    harness
        .llm
        .engine("searxng", Ok(response("searxng", "Local search result")));
    harness.llm.script(
        &id,
        tool_reply(&[("search1", "web_search", r#"{"query":"release"}"#)]),
    );
    harness.llm.script(&id, text_reply("Done."));
    harness.submit(&id, "Search release").await;
    harness.settle(&id).await;

    assert_eq!(
        *harness.llm.search_base_urls.lock().unwrap(),
        vec![Some("https://search.example".into())]
    );
}

#[tokio::test]
async fn unconfigured_engines_are_skipped_silently() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    configure_chain(&harness, "web/exa, web/duckduckgo").await;
    harness
        .llm
        .providers
        .lock()
        .unwrap()
        .push("duckduckgo".into());
    harness
        .llm
        .engine("duckduckgo", Ok(response("duckduckgo", "Found it")));
    harness.llm.script(
        &id,
        tool_reply(&[("search1", "web_search", r#"{"query":"release"}"#)]),
    );
    harness.llm.script(&id, text_reply("Search completed."));
    harness.submit(&id, "Search release").await;
    harness.settle(&id).await;

    assert_eq!(
        *harness.llm.search_calls.lock().unwrap(),
        vec!["duckduckgo"]
    );
    assert!(
        !tool_results(&harness.llm.requests_for(&id)[1])
            .iter()
            .any(|text| text.contains("exa:"))
    );
}

#[tokio::test]
async fn all_provider_failures_are_listed_in_chain_order() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    configure_chain(&harness, "web/exa, web/duckduckgo").await;
    harness
        .llm
        .providers
        .lock()
        .unwrap()
        .extend(["exa".into(), "duckduckgo".into()]);
    harness.llm.engine(
        "exa",
        Err(roost_llm::LlmError::Network("first failure".into())),
    );
    harness.llm.engine(
        "duckduckgo",
        Err(roost_llm::LlmError::Network("second failure".into())),
    );
    harness.llm.script(
        &id,
        tool_reply(&[("search1", "web_search", r#"{"query":"release"}"#)]),
    );
    harness
        .llm
        .script(&id, text_reply("The search providers are unavailable."));
    harness.submit(&id, "Search release").await;
    harness.settle(&id).await;

    let results = tool_results(&harness.llm.requests_for(&id)[1]);
    let message = results.first().expect("search tool result");
    assert!(message.contains("All web search providers failed"));
    assert!(message.find("Exa").unwrap() < message.find("DuckDuckGo").unwrap());
    assert!(message.contains("first failure"));
    assert!(message.contains("second failure"));
}

#[tokio::test]
async fn query_constraint_relaxes_when_it_would_remove_every_source() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    configure_chain(&harness, "web/duckduckgo").await;
    harness
        .llm
        .providers
        .lock()
        .unwrap()
        .push("duckduckgo".into());
    harness
        .llm
        .engine("duckduckgo", Ok(response("duckduckgo", "Relaxed result")));
    harness.llm.script(
        &id,
        tool_reply(&[(
            "search1",
            "web_search",
            r#"{"query":"site:docs.example topic"}"#,
        )]),
    );
    harness.llm.script(
        &id,
        text_reply("Search completed with relaxed constraints."),
    );
    harness.submit(&id, "Search docs").await;
    harness.settle(&id).await;

    let results = tool_results(&harness.llm.requests_for(&id)[1]);
    assert!(results[0].contains("Relaxed site:docs.example"));
}

async fn configure_chain(harness: &Harness, chain: &str) {
    harness
        .runtime
        .set_settings(AgentSettings {
            model_roles: BTreeMap::from([(Role::Web, chain.to_owned())]),
            ..AgentSettings::default()
        })
        .await
        .unwrap();
}

fn response(provider: &str, answer: &str) -> SearchResponse {
    SearchResponse {
        provider: provider.into(),
        answer: Some(answer.into()),
        sources: vec![SearchSource {
            title: "Result".into(),
            url: "https://example.com/result".into(),
            snippet: Some("A useful result".into()),
            published: None,
            age_seconds: None,
        }],
        citations: Vec::new(),
        related_questions: Vec::new(),
        search_queries: Vec::new(),
    }
}

/// The text of every tool result the request sends back to the model.
fn tool_results(request: &roost_llm::ChatRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            roost_llm::Message::ToolResult { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}
