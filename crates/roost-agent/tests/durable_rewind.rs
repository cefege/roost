//! Native Durable regressions for rewind and compaction journal synchronization.
//! These exercise the opened engine through the provider request boundary.
//! Legacy journal fixtures place compaction summaries without invoking a model.

#[path = "support/mod.rs"]
mod support;

use roost_agent::{AgentStore, Entry};
use roost_llm::{AssistantBlock, Usage};
use roost_protocol::wire::agent_chat::TranscriptItem;
use support::{Harness, text_reply, user_texts};

#[tokio::test]
async fn rewind_before_first_sync_survives_restart_and_keeps_earlier_turn() {
    let harness = Harness::new();
    let conversation_id = harness.conversation().await;
    for (prompt, answer) in [("user A", "answer A"), ("user B", "answer B")] {
        harness.llm.script(&conversation_id, text_reply(answer));
        harness.submit(&conversation_id, prompt).await;
        harness.settle(&conversation_id).await;
    }

    let user_b_id = projected_user_id(&harness, &conversation_id, "user B").await;
    harness
        .runtime
        .rewind(&conversation_id, &user_b_id, false)
        .await
        .unwrap();
    let restarted = harness.restart_runtime();
    harness.llm.script(&conversation_id, text_reply("answer C"));
    restarted
        .submit(&conversation_id, "user C".into())
        .await
        .unwrap();
    let transcript = harness.settle_runtime(&restarted, &conversation_id).await;

    let request = harness.llm.requests_for(&conversation_id).pop().unwrap();
    assert_eq!(user_texts(&request), vec!["user A", "user C"]);
    let context = request_text(&request);
    assert!(context.contains("answer A"));
    assert!(!context.contains("user B"));
    assert!(!context.contains("answer B"));
    assert!(
        support::texts(&transcript)
            .iter()
            .any(|text| text == "answer C")
    );
}

#[tokio::test]
async fn rewind_before_compaction_restores_older_provider_history() {
    let harness = Harness::new();
    let conversation_id = harness.conversation().await;
    append_legacy_turn(&harness, &conversation_id, "older user", "older answer").await;
    append_legacy_turn(
        &harness,
        &conversation_id,
        "discarded user",
        "discarded answer",
    )
    .await;
    let entries = harness.store.entries(&conversation_id).await.unwrap();
    let discarded_seq = entries
        .iter()
        .find_map(|(seq, entry)| {
            matches!(
                entry, Entry::User { text, .. } if text == "discarded user"
            )
            .then_some(*seq)
        })
        .unwrap();
    harness
        .store
        .append_entry(
            &conversation_id,
            &Entry::Compaction {
                summary: "discarded summary".into(),
                first_kept_seq: discarded_seq,
            },
            &[],
            None,
        )
        .await
        .unwrap();
    harness
        .llm
        .script(&conversation_id, text_reply("later answer"));
    harness.submit(&conversation_id, "later user").await;
    harness.settle(&conversation_id).await;

    let discarded_id = projected_user_id(&harness, &conversation_id, "discarded user").await;
    harness
        .runtime
        .rewind(&conversation_id, &discarded_id, false)
        .await
        .unwrap();
    harness
        .llm
        .script(&conversation_id, text_reply("new answer"));
    let restarted = harness.restart_runtime();
    restarted
        .submit(&conversation_id, "new user".into())
        .await
        .unwrap();
    harness.settle_runtime(&restarted, &conversation_id).await;

    let request = harness.llm.requests_for(&conversation_id).pop().unwrap();
    assert_eq!(user_texts(&request), vec!["older user", "new user"]);
    let context = request_text(&request);
    assert!(context.contains("older answer"));
    assert!(!context.contains("discarded summary"));
    assert!(!context.contains("discarded answer"));
}

#[tokio::test]
async fn rewind_after_compaction_keeps_summary_once_after_restart() {
    let harness = Harness::new();
    let conversation_id = harness.conversation().await;
    append_legacy_turn(
        &harness,
        &conversation_id,
        "summarized user",
        "summarized answer",
    )
    .await;
    let entries = harness.store.entries(&conversation_id).await.unwrap();
    let first_kept_seq = entries.last().map(|(seq, _)| seq + 1).unwrap();
    harness
        .store
        .append_entry(
            &conversation_id,
            &Entry::Compaction {
                summary: "retained summary".into(),
                first_kept_seq,
            },
            &[],
            None,
        )
        .await
        .unwrap();
    harness
        .llm
        .script(&conversation_id, text_reply("later answer"));
    harness.submit(&conversation_id, "later user").await;
    harness.settle(&conversation_id).await;

    let later_id = projected_user_id(&harness, &conversation_id, "later user").await;
    harness
        .runtime
        .rewind(&conversation_id, &later_id, false)
        .await
        .unwrap();
    let restarted = harness.restart_runtime();
    harness
        .llm
        .script(&conversation_id, text_reply("new answer"));
    restarted
        .submit(&conversation_id, "new user".into())
        .await
        .unwrap();
    let transcript = harness.settle_runtime(&restarted, &conversation_id).await;

    let request = harness.llm.requests_for(&conversation_id).pop().unwrap();
    let context = request_text(&request);
    assert_eq!(context.matches("retained summary").count(), 1);
    assert!(!context.contains("summarized answer"));
    assert!(!context.contains("later answer"));
    assert!(context.contains("new user"));
    assert!(
        support::texts(&transcript)
            .iter()
            .any(|text| text == "new answer")
    );
}

async fn append_legacy_turn(harness: &Harness, conversation_id: &str, prompt: &str, answer: &str) {
    harness
        .store
        .append_entry(
            conversation_id,
            &Entry::User {
                text: prompt.into(),
                loop_repeat: false,
                item_id: Some(prompt.into()),
            },
            &[],
            None,
        )
        .await
        .unwrap();
    harness
        .store
        .append_entry(
            conversation_id,
            &Entry::Assistant {
                item_id: format!("{prompt}-assistant"),
                blocks: vec![AssistantBlock::Text {
                    text: answer.into(),
                    provider_data: None,
                }],
                usage: Usage::default(),
                model: None,
                metadata: None,
            },
            &[],
            None,
        )
        .await
        .unwrap();
}

async fn projected_user_id(harness: &Harness, conversation_id: &str, text: &str) -> String {
    harness
        .runtime
        .transcript(conversation_id)
        .await
        .unwrap()
        .items
        .into_iter()
        .find_map(|item| match item {
            TranscriptItem::User {
                id,
                text: item_text,
                ..
            } if item_text == text => Some(id),
            _ => None,
        })
        .unwrap()
}

fn request_text(request: &roost_llm::ChatRequest) -> String {
    request
        .messages
        .iter()
        .map(|message| match message {
            roost_llm::Message::User { content } => content
                .iter()
                .filter_map(|part| match part {
                    roost_llm::UserContent::Text { text } => Some(text.as_str()),
                    roost_llm::UserContent::Image { .. } => None,
                })
                .collect::<String>(),
            roost_llm::Message::Assistant { blocks, .. } => blocks
                .iter()
                .filter_map(|block| match block {
                    AssistantBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
            roost_llm::Message::ToolResult { text, .. } => text.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
