//! Loop completion checkpoints and repeat-origin accounting across recovery.
//! The parent recovery suite reconstructs the runtime over the durable journal.
//! Saved responses and identical user steering must not consume extra runs.

use roost_agent::records::{AssistantMetadata, model_ref};
use roost_agent::{AgentStore, Entry};
use roost_llm::{AssistantBlock, StopReason, Usage};

use super::{Harness, LIMIT_REACHED, loop_status, notice_bodies, text_reply, user_texts};

async fn append_recovery_entry(harness: &Harness, id: &str, entry: Entry) -> u64 {
    harness
        .store
        .append_entry(id, &entry, &[], None)
        .await
        .unwrap()
}

#[tokio::test]
async fn recovered_final_output_admits_the_next_loop_iteration() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    append_recovery_entry(
        &harness,
        &id,
        Entry::LoopState {
            state_json: Some(
                serde_json::json!({
                    "body": "bump", "paused": false,
                    "budget": {"kind": "runs", "remaining": 1},
                    "condition": null, "pending_repeat": null,
                    "condition_pending": false, "condition_ready": false
                })
                .to_string(),
            ),
        },
    )
    .await;
    let input_seq = append_recovery_entry(
        &harness,
        &id,
        Entry::User {
            text: "bump".into(),
            loop_repeat: false,
            item_id: None,
        },
    )
    .await;
    append_recovery_entry(
        &harness,
        &id,
        Entry::RunIntent {
            active: true,
            through_seq: input_seq,
            aborted: false,
            yielded: false,
        },
    )
    .await;
    append_recovery_entry(
        &harness,
        &id,
        Entry::Assistant {
            item_id: "first-loop-answer".into(),
            blocks: vec![AssistantBlock::Text {
                text: "first answer".into(),
                provider_data: None,
            }],
            usage: Usage::default(),
            model: Some(model_ref(
                super::super::support::PROVIDER,
                super::super::support::MODEL,
            )),
            metadata: Some(AssistantMetadata {
                completed: true,
                stop_reason: Some(StopReason::MaxTokens),
                input_through_seq: input_seq,
                continued: false,
                yield_reminders: 0,
                goal_id: None,
            }),
        },
    )
    .await;
    harness.llm.script(&id, text_reply("second answer"));
    let restarted = harness.restart_runtime();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 1);
    let transcript = harness.settle_runtime(&restarted, &id).await;
    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 1);
    assert_eq!(user_texts(&requests[0]), vec!["bump", "bump"]);
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| body == LIMIT_REACHED)
    );
}

#[tokio::test]
async fn an_identical_user_steer_is_not_charged_as_a_committed_repeat() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    append_recovery_entry(
        &harness,
        &id,
        Entry::LoopState {
            state_json: Some(
                serde_json::json!({
                    "body": "bump", "paused": false,
                    "budget": {"kind": "runs", "remaining": 2},
                    "condition": null,
                    "pending_repeat": {"after_seq": 0, "body": "bump"},
                    "condition_pending": false, "condition_ready": false
                })
                .to_string(),
            ),
        },
    )
    .await;
    append_recovery_entry(
        &harness,
        &id,
        Entry::User {
            text: "bump".into(),
            loop_repeat: false,
            item_id: None,
        },
    )
    .await;
    for reply in [
        "steer handled",
        "first permitted repeat",
        "last permitted repeat",
    ] {
        harness.llm.script(&id, text_reply(reply));
    }
    let restarted = harness.restart_runtime();
    restarted.recover_interrupted_runs().await.unwrap();
    let transcript = harness.settle_runtime(&restarted, &id).await;
    let requests = harness.llm.requests_for(&id);
    assert_eq!(
        requests.len(),
        3,
        "only the two loop-origin commits spend the two remaining runs"
    );
    assert_eq!(user_texts(&requests[2]), vec!["bump", "bump", "bump"]);
    assert!(loop_status(&transcript.items).is_none());
}
