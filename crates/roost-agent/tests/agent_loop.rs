//! The conversation loop against scripted seams: tool rounds, steering,
//! abort, account rotation, compaction, commands, auto thinking and
//! unexpected-stop continuation.

#[path = "support/mod.rs"]
mod support;

use std::collections::BTreeMap;
use std::time::Duration;

use roost_agent::{Entry, history};
use roost_llm::{Answer, LlmError};
use roost_protocol::wire::agent_chat::{AgentRunState, TranscriptItem};
use support::{Harness, text_reply, tool_reply, user_texts};

#[tokio::test]
async fn a_tool_call_runs_on_the_worker_and_its_result_reaches_the_next_request() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.tools.answer("read", "[a.txt#5BF9]\n1:hello");
    harness.llm.script(
        &id,
        tool_reply(&[("call-1", "read", r#"{"path":"a.txt"}"#)]),
    );
    harness.llm.script(&id, text_reply("The file says hello."));
    harness.submit(&id, "what does a.txt say?").await;
    let transcript = harness.settle(&id).await;

    assert_eq!(transcript.run_state, AgentRunState::Idle);
    let calls = harness.tools.calls_to("read");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cwd, "/repo");
    assert!(transcript.items.iter().any(|item| matches!(item,
        TranscriptItem::Tool { tool_name, output, running: false, .. } if tool_name == "read" && output.contains("1:hello"))));
    let second = &harness.llm.requests_for(&id)[1];
    assert!(second.messages.iter().any(|message| matches!(message,
        roost_llm::Message::ToolResult { call_id, text, .. } if call_id == "call-1" && text.contains("hello"))));
    assert_eq!(
        support::texts(&transcript).last().map(String::as_str),
        Some("The file says hello.")
    );
}

#[tokio::test]
async fn a_message_sent_during_a_run_is_injected_after_the_tool_round() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    *harness.tools.delay.lock().unwrap() = Some(Duration::from_millis(200));
    harness.llm.script(
        &id,
        tool_reply(&[("call-1", "bash", r#"{"command":"make"}"#)]),
    );
    harness.llm.script(&id, text_reply("done, and noted"));
    harness.submit(&id, "build it").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    harness.submit(&id, "also run the tests").await;
    harness.settle(&id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(
        requests.len(),
        2,
        "the steer joins the running loop, not a second run"
    );
    assert!(
        user_texts(&requests[1])
            .iter()
            .any(|text| text == "also run the tests")
    );
}

#[tokio::test]
async fn abort_cancels_a_running_tool_and_leaves_the_conversation_idle() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    *harness.tools.delay.lock().unwrap() = Some(Duration::from_secs(30));
    harness.llm.script(
        &id,
        tool_reply(&[("call-1", "bash", r#"{"command":"sleep 30"}"#)]),
    );
    harness.submit(&id, "wait").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(harness.runtime.abort(&id));
    let transcript = harness.settle(&id).await;
    assert_eq!(transcript.run_state, AgentRunState::Idle);
    assert_eq!(harness.llm.requests_for(&id).len(), 1);
}

#[tokio::test]
async fn a_rate_limited_call_rotates_to_another_account_and_succeeds() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(
        &id,
        Err(LlmError::RateLimited {
            retry_after_ms: Some(1000),
            reset_at_ms: None,
        }),
    );
    harness
        .llm
        .script(&id, text_reply("served by the second account"));
    harness.submit(&id, "hi").await;
    let transcript = harness.settle(&id).await;
    assert_eq!(transcript.run_state, AgentRunState::Idle);
    assert_eq!(*harness.llm.rotations.lock().unwrap(), vec![1]);
    assert_eq!(
        support::texts(&transcript).last().map(String::as_str),
        Some("served by the second account")
    );
}

#[tokio::test]
async fn a_hard_provider_error_fails_the_run_with_its_message() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(
        &id,
        Err(LlmError::Http {
            status: 400,
            body: "bad request".into(),
        }),
    );
    harness.submit(&id, "hi").await;
    let transcript = harness.settle(&id).await;
    assert_eq!(transcript.run_state, AgentRunState::Failed);
    assert!(transcript.error.unwrap_or_default().contains("bad request"));
}

#[tokio::test]
async fn usage_near_the_context_window_compacts_before_the_next_call() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let window = harness
        .runtime
        .catalog()
        .get(support::PROVIDER, support::MODEL)
        .unwrap()
        .context_window;
    // Enough history that the newest 20 000 tokens leave something to summarise.
    for turn in 0..3 {
        harness
            .store_entry(
                &id,
                Entry::User {
                    text: format!("question {turn} {}", "x".repeat(40_000)),
                },
            )
            .await;
        harness
            .store_entry(
                &id,
                Entry::Assistant {
                    item_id: format!("a{turn}"),
                    blocks: vec![roost_llm::AssistantBlock::Text {
                        text: "y".repeat(40_000),
                    }],
                    usage: roost_llm::Usage {
                        input: window - 1000,
                        output: 10,
                        cache_read: 0,
                        cache_write: 0,
                    },
                    model: Some(roost_agent::records::model_ref(
                        support::PROVIDER,
                        support::MODEL,
                    )),
                },
            )
            .await;
    }
    harness
        .llm
        .script(&id, text_reply("SUMMARY OF EARLIER WORK"));
    harness
        .llm
        .script(&id, text_reply("answer after compaction"));
    harness.submit(&id, "next question").await;
    harness.settle(&id).await;

    let entries = harness.runtime_entries(&id).await;
    assert!(entries.iter().any(|(_, entry)| matches!(entry, Entry::Compaction { summary, .. } if summary.contains("SUMMARY"))));
    let last = harness.llm.requests_for(&id).pop().unwrap();
    assert!(user_texts(&last)[0].contains("SUMMARY OF EARLIER WORK"));
    assert!(
        !user_texts(&last)
            .iter()
            .any(|text| text.starts_with("question 0"))
    );
    let (summary, _) = history::active_window(&entries);
    assert!(summary.is_some());
}

#[tokio::test]
async fn usage_command_answers_with_a_notice_and_no_model_call() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .llm
        .usage_rows
        .lock()
        .unwrap()
        .push(roost_agent::AccountUsage {
            provider: "anthropic".into(),
            label: "me@example.com".into(),
            windows: vec![roost_llm::UsageWindow {
                name: "5h".into(),
                used_fraction: 0.42,
                resets_at_ms: None,
            }],
            note: None,
            blocked_until_ms: None,
            disabled_cause: None,
        });
    harness.submit(&id, "/usage").await;
    let transcript = harness.runtime.transcript(&id).await.unwrap();
    assert!(harness.llm.requests.lock().unwrap().is_empty());
    assert!(transcript.items.iter().any(|item| matches!(item,
        TranscriptItem::Notice { body, .. } if body.contains("me@example.com") && body.contains("5h 42%"))));
}

#[tokio::test]
async fn a_path_that_looks_like_a_command_is_sent_as_text() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .llm
        .script(&id, text_reply("that is the hosts file"));
    harness.submit(&id, "/etc/hosts").await;
    harness.settle(&id).await;
    let request = &harness.llm.requests_for(&id)[0];
    assert_eq!(user_texts(request), vec!["/etc/hosts".to_owned()]);
}

#[tokio::test]
async fn auto_thinking_uses_the_level_the_judge_chooses() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .runtime
        .set_thinking_level(&id, "auto".into())
        .await
        .unwrap();
    harness.llm.judgment(Ok(BTreeMap::from([(
        "level".to_owned(),
        Answer::Choice {
            choice: "high".into(),
            probabilities: BTreeMap::new(),
            confidence: 0.9,
        },
    )])));
    harness.llm.script(&id, text_reply("thought hard"));
    harness.submit(&id, "find the race in the cache").await;
    harness.settle(&id).await;
    assert_eq!(harness.llm.requests_for(&id)[0].thinking, "high");
}

#[tokio::test]
async fn an_unexpected_stop_is_continued_exactly_once() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let stopped = || {
        Ok(BTreeMap::from([(
            "stopped".to_owned(),
            Answer::Bool { probability: 0.9 },
        )]))
    };
    harness.llm.judgment(stopped());
    harness.llm.judgment(stopped());
    harness
        .llm
        .script(&id, text_reply("Let me run the tests next."));
    harness.llm.script(&id, text_reply("I'll fix that now."));
    harness.submit(&id, "fix the bug").await;
    harness.settle(&id).await;
    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 2);
    assert!(
        user_texts(&requests[1])
            .last()
            .unwrap()
            .contains("Continue: you stopped before finishing")
    );
}

impl Harness {
    async fn store_entry(&self, id: &str, entry: Entry) {
        use roost_agent::AgentStore;
        self.store.append_entry(id, &entry).await.unwrap();
    }

    async fn runtime_entries(&self, id: &str) -> Vec<(u64, Entry)> {
        use roost_agent::AgentStore;
        self.store.entries(id).await.unwrap()
    }
}
