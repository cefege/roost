//! Fetch and search harness tool exposure and result delivery.

#[path = "support/mod.rs"]
mod support;

use roost_agent::{FetchedPage, SearchHit};
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
        tool_reply(&[(
            "fetch-page",
            "fetch",
            r#"{"url":"https://example.com/docs"}"#,
        )]),
    );
    harness.llm.script(&id, text_reply("The limit is 42."));
    harness.submit(&id, "Find the documented limit").await;
    harness.settle(&id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 2);
    assert!(
        tool_results(&requests[1])
            .iter()
            .any(|text| text.contains("The documented limit is 42."))
    );
}

#[tokio::test]
async fn web_search_is_offered_only_with_an_exa_account() {
    let harness = Harness::new();
    let without_exa = harness.conversation().await;
    harness
        .llm
        .script(&without_exa, text_reply("No search credential."));
    harness.submit(&without_exa, "Answer without search").await;
    harness.settle(&without_exa).await;
    assert!(
        harness.llm.requests_for(&without_exa)[0]
            .tools
            .iter()
            .all(|tool| tool.name != "web_search")
    );

    harness.llm.providers.lock().unwrap().push("exa".into());
    let with_exa = harness.conversation().await;
    harness.llm.search(
        "current roost release",
        vec![SearchHit {
            title: "Release notes".into(),
            url: "https://example.com/release".into(),
            snippet: "Latest release details".into(),
            published: None,
        }],
    );
    harness.llm.script(
        &with_exa,
        tool_reply(&[(
            "search",
            "web_search",
            r#"{"query":"current roost release"}"#,
        )]),
    );
    harness
        .llm
        .script(&with_exa, text_reply("The release notes say…"));
    harness
        .submit(&with_exa, "Search for current release details")
        .await;
    harness.settle(&with_exa).await;
    let requests = harness.llm.requests_for(&with_exa);
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == "web_search")
    );
    assert!(
        tool_results(&requests[1])
            .iter()
            .any(|text| text.contains("Latest release details"))
    );
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
