//! Incomplete responses resume without provider-specific assistant prefill.
//! The parent suite shares journal helpers and scripted provider seams.
//! Partial progress remains context without re-consuming the accepted prompt.

use super::support::{Harness, text_reply};
use super::{append_journal_entry, assistant_entry};
use roost_agent::Entry;
use roost_agent::records::AssistantMetadata;
use roost_llm::{AssistantBlock, Message};

#[tokio::test]
async fn recovered_partial_assistant_output_retries_the_primary_call() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let input_seq = append_journal_entry(
        &harness,
        &id,
        Entry::User {
            text: "retry this response".into(),
            loop_repeat: false,
        },
    )
    .await;
    append_journal_entry(
        &harness,
        &id,
        Entry::RunIntent {
            active: true,
            through_seq: input_seq,
            aborted: false,
        },
    )
    .await;
    append_journal_entry(
        &harness,
        &id,
        assistant_entry(
            "partial-answer",
            "partial text",
            Some(AssistantMetadata {
                completed: false,
                stop_reason: None,
                input_through_seq: input_seq,
                continued: false,
                yield_reminders: 0,
                goal_id: None,
            }),
        ),
    )
    .await;
    harness.llm.script(&id, text_reply("complete answer"));

    let restarted = harness.restart_runtime();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 1);
    harness.settle_runtime(&restarted, &id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 1);
    assert!(matches!(
        requests[0].messages.last(),
        Some(Message::User { .. })
    ));
    assert!(requests[0].messages.iter().any(|message| matches!(
        message,
        Message::Assistant { blocks } if blocks.iter().any(|block| matches!(
            block, AssistantBlock::Text { text } if text == "partial text"
        ))
    )));
}
