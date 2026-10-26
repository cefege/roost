//! Native forks preserve Pi's as-of conversation document state.
//! Scripts write and read the real code-mode store across independent kernel
//! namespaces, including a historical fork after the source has advanced.

mod support;

use roost_agent::{AgentStore, Entry};
use roost_llm::Message;
use support::{Harness, text_reply, tool_reply};

#[tokio::test]
async fn historical_and_current_forks_preserve_independent_code_mode_store() {
    let harness = Harness::new();
    let source_id = harness.conversation().await;
    script_turn(
        &harness,
        &source_id,
        "store-first",
        "store('checkpoint', 6);",
        "First checkpoint.",
    )
    .await;
    let boundary = harness
        .store
        .entries(&source_id)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|(_, entry)| match entry {
            Entry::Assistant { item_id, .. } => Some(item_id),
            _ => None,
        })
        .unwrap();
    script_turn(
        &harness,
        &source_id,
        "store-second",
        "store('checkpoint', 19);",
        "Second checkpoint.",
    )
    .await;

    let historical = harness
        .runtime
        .fork(&source_id, Some(&boundary))
        .await
        .unwrap();
    let current = harness.runtime.fork(&source_id, None).await.unwrap();
    assert_stored_value(&harness, &historical.id, "read-historical", 6, true).await;
    assert_stored_value(&harness, &current.id, "read-current", 19, false).await;
    assert_stored_value(&harness, &source_id, "read-source", 19, false).await;
}

async fn script_turn(harness: &Harness, id: &str, call_id: &str, code: &str, prompt: &str) {
    let args = serde_json::json!({"code": code}).to_string();
    harness
        .llm
        .script(id, tool_reply(&[(call_id, "codemode", &args)]));
    harness.llm.script(id, text_reply("Script completed."));
    harness.submit(id, prompt).await;
    harness.settle(id).await;
}

async fn assert_stored_value(
    harness: &Harness,
    id: &str,
    call_id: &str,
    expected: u64,
    mutate: bool,
) {
    let code = if mutate {
        "text('checkpoint=' + load('checkpoint')); store('checkpoint', 42);"
    } else {
        "text('checkpoint=' + load('checkpoint'));"
    };
    script_turn(harness, id, call_id, code, "Read the saved checkpoint.").await;
    let requests = harness.llm.requests_for(id);
    let result = requests
        .iter()
        .rev()
        .find_map(|request| {
            request.messages.iter().find_map(|message| match message {
                Message::ToolResult {
                    call_id: actual,
                    content: None,
                    text,
                    is_error: false,
                    ..
                } if actual == call_id => Some(text.clone()),
                Message::ToolResult {
                    call_id: actual,
                    content: Some(content),
                    is_error: false,
                    ..
                } if actual == call_id => Some(
                    content
                        .iter()
                        .filter_map(|item| match item {
                            roost_llm::UserContent::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
        })
        .expect("the actual saved-state script result reaches the following model call");
    assert!(
        result.contains(&format!("checkpoint={expected}")),
        "unexpected store result: {result}"
    );
}
