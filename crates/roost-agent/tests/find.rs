//! Semantic find cascade behavior through scripted model, judge, and worker seams.

#[path = "support/mod.rs"]
mod support;

use std::collections::BTreeMap;
use std::time::Duration;

use roost_agent::{Role, records::AgentSettings};
use roost_llm::{Answer, LlmError};
use roost_protocol::wire::agent_chat::{AgentRunState, TranscriptItem};
use support::{Harness, text_reply, tool_reply};

fn yes(key: &str, probability: f64) -> BTreeMap<String, Answer> {
    BTreeMap::from([(key.to_owned(), Answer::Bool { probability })])
}

async fn native_harness() -> (Harness, String) {
    let harness = Harness::new();
    harness
        .llm
        .providers
        .lock()
        .unwrap()
        .push("typesafe".into());
    harness
        .runtime
        .set_settings(AgentSettings {
            model_roles: BTreeMap::from([(Role::Judge, "typesafe/jev-latest".into())]),
            ..AgentSettings::default()
        })
        .await
        .unwrap();
    let id = harness.conversation().await;
    (harness, id)
}

#[tokio::test]
async fn semantic_find_returns_judged_line_range() {
    let (harness, id) = native_harness().await;
    harness.tools.answer("glob", "src/main.rs");
    harness.tools.answer(
        "read",
        "[src/main.rs#1234]\n1: fn main() {\n2:     semantic_search();\n3: }",
    );
    harness.llm.script(
        &id,
        tool_reply(&[("c1", "find", r#"{"query":"semantic search"}"#)]),
    );
    harness.llm.script(&id, text_reply("Found it."));
    harness.llm.judgment(Ok(yes("e000", 0.9)));
    harness.llm.judgment(Ok(yes("p00", 0.9)));
    harness.llm.judgment(Ok(yes("p00", 0.8)));
    harness
        .submit(&id, "find the semantic search implementation")
        .await;
    let transcript = harness.settle(&id).await;
    assert_eq!(transcript.run_state, AgentRunState::Idle);
    assert!(
        transcript.items.iter().any(|item| matches!(item,
            TranscriptItem::Tool { tool_name, output, running: false, .. }
            if tool_name == "find" && output.contains("src/main.rs:1-3"))),
        "transcript: {:?}",
        transcript.items
    );
    let requests = harness.llm.requests_for(&id);
    assert!(requests[0].tools.iter().any(|tool| tool.name == "find"));
    assert!(
        harness.tools.calls_to("read")[0]
            .args_json
            .contains("\"limit\":200")
    );
}

#[tokio::test]
async fn failed_sketch_judgment_keeps_candidate_as_unknown() {
    let (harness, id) = native_harness().await;
    harness.tools.answer("glob", "src/lib.rs");
    harness.tools.answer(
        "read",
        "[src/lib.rs#1234]\n4: fn durable_cache() {}\n5: cache.write();",
    );
    harness.llm.script(
        &id,
        tool_reply(&[("c1", "find", r#"{"query":"durable cache"}"#)]),
    );
    harness.llm.script(&id, text_reply("Found it."));
    harness.llm.judgment(Ok(yes("e000", 0.9)));
    harness
        .llm
        .judgment(Err(LlmError::Decode("temporary judge failure".into())));
    harness.llm.judgment(Ok(yes("p00", 0.8)));
    harness
        .submit(&id, "find the durable cache implementation")
        .await;
    let transcript = harness.settle(&id).await;
    assert!(transcript.items.iter().any(|item| matches!(item,
        TranscriptItem::Tool { tool_name, output, .. }
        if tool_name == "find" && output.contains("src/lib.rs:4-5"))));
}

#[tokio::test]
async fn timeout_returns_partial_result_and_completes_find_tool() {
    let base = Harness::new();
    base.llm.providers.lock().unwrap().push("typesafe".into());
    base.runtime
        .set_settings(AgentSettings {
            model_roles: BTreeMap::from([(Role::Judge, "typesafe/jev-latest".into())]),
            ..AgentSettings::default()
        })
        .await
        .unwrap();
    let runtime = roost_agent::AgentRuntime::new(
        base.store.clone(),
        base.tools.clone(),
        base.sink.clone(),
        base.llm.clone(),
        roost_agent::RuntimeConfig {
            find_timeout: Duration::from_millis(20),
            ..roost_agent::RuntimeConfig::default()
        },
    );
    let harness = Harness { runtime, ..base };
    let id = harness.conversation().await;
    harness.tools.answer("glob", "src/lib.rs");
    *harness.tools.delay.lock().unwrap() = Some(Duration::from_millis(100));
    harness
        .llm
        .script(&id, tool_reply(&[("c1", "find", r#"{"query":"cache"}"#)]));
    harness.llm.script(&id, text_reply("Search completed."));
    harness.submit(&id, "find cache").await;
    let transcript = harness.settle(&id).await;
    assert!(transcript.items.iter().any(|item| matches!(item,
        TranscriptItem::Tool { tool_name, output, .. }
        if tool_name == "find" && output.contains("No matching passages found."))));
}

#[tokio::test]
async fn chat_only_judge_does_not_offer_find() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .llm
        .script(&id, text_reply("No semantic search available."));
    harness.submit(&id, "search").await;
    harness.settle(&id).await;
    assert!(
        !harness.llm.requests_for(&id)[0]
            .tools
            .iter()
            .any(|tool| tool.name == "find")
    );
}
