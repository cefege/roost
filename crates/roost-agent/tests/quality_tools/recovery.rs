//! Accepted-input sequences, not late intent writes, govern response consumption.
//! The parent quality-tool suite shares its persistent journal fixture.
//! Recovery must preserve completed output already representing a late steer.

use super::support::{Harness, text_reply};
use roost_agent::records::AssistantMetadata;
use roost_agent::{AgentStore, Entry};
use roost_llm::{AssistantBlock, StopReason, Usage};

#[tokio::test]
async fn late_run_intent_does_not_discard_already_consumed_output() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let first_seq = harness
        .store
        .append_entry(
            &id,
            &Entry::User {
                text: "first".into(),
                loop_repeat: false,
            },
            &[],
        )
        .await
        .unwrap();
    harness
        .store
        .append_entry(
            &id,
            &Entry::RunIntent {
                active: true,
                through_seq: first_seq,
                aborted: false,
            },
            &[],
        )
        .await
        .unwrap();
    let consumed_seq = harness
        .store
        .append_entry(
            &id,
            &Entry::User {
                text: "already consumed".into(),
                loop_repeat: false,
            },
            &[],
        )
        .await
        .unwrap();
    harness
        .store
        .append_entry(
            &id,
            &Entry::Assistant {
                item_id: "completed-output".into(),
                blocks: vec![AssistantBlock::Text {
                    text: "completed".into(),
                }],
                usage: Usage::default(),
                model: None,
                metadata: Some(AssistantMetadata {
                    completed: true,
                    stop_reason: Some(StopReason::MaxTokens),
                    input_through_seq: consumed_seq,
                    continued: false,
                    yield_reminders: 0,
                    goal_id: None,
                }),
            },
            &[],
        )
        .await
        .unwrap();
    harness
        .store
        .append_entry(
            &id,
            &Entry::RunIntent {
                active: true,
                through_seq: consumed_seq,
                aborted: false,
            },
            &[],
        )
        .await
        .unwrap();
    harness.llm.script(&id, text_reply("must not repeat"));

    let restarted = harness.restart_runtime();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 1);
    harness.settle_runtime(&restarted, &id).await;

    assert!(harness.llm.requests_for(&id).is_empty());
}
