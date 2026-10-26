//! Recovery resumes post-turn policy without replaying the completed model call.
//! The parent suite shares a durable store and scripted provider/judge seams.
//! Continuation memory survives a restart after a successful response commit.

use super::support::{self, Harness, text_reply, user_texts};
use roost_agent::records::{AssistantMetadata, model_ref};
use roost_agent::{AgentStore, Entry};
use roost_llm::{Answer, AssistantBlock, StopReason, Usage};
use std::collections::BTreeMap;

#[tokio::test]
async fn recovery_replays_unexpected_stop_policy_once_for_completed_output() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let input_seq = harness
        .store
        .append_entry(
            &id,
            &Entry::User {
                text: "fix the bug".into(),
                loop_repeat: false,
                item_id: None,
            },
            &[],
            None,
        )
        .await
        .unwrap();
    harness
        .store
        .append_entry(
            &id,
            &Entry::RunIntent {
                active: true,
                through_seq: input_seq,
                aborted: false,
                yielded: false,
            },
            &[],
            None,
        )
        .await
        .unwrap();
    harness
        .store
        .append_entry(
            &id,
            &Entry::Assistant {
                item_id: "completed-stop".into(),
                blocks: vec![AssistantBlock::Text {
                    text: "I'll fix that now.".into(),
                    provider_data: None,
                }],
                usage: Usage::default(),
                model: Some(model_ref(support::PROVIDER, support::MODEL)),
                metadata: Some(AssistantMetadata {
                    completed: true,
                    stop_reason: Some(StopReason::EndTurn),
                    input_through_seq: input_seq,
                    continued: false,
                    yield_reminders: 0,
                    goal_id: None,
                }),
            },
            &[],
            None,
        )
        .await
        .unwrap();
    harness.llm.judgment(Ok(BTreeMap::from([(
        "stopped".to_owned(),
        Answer::Bool { probability: 0.9 },
    )])));
    harness.llm.script(&id, text_reply("I'll fix that now."));

    let restarted = harness.restart_runtime();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 1);
    harness.settle_runtime(&restarted, &id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 1);
    assert!(
        user_texts(&requests[0])
            .last()
            .is_some_and(|text| text.contains("Continue: you stopped before finishing"))
    );
    assert!(
        harness
            .llm
            .judge_requests
            .lock()
            .is_ok_and(|requests| requests.len() == 1)
    );
}
