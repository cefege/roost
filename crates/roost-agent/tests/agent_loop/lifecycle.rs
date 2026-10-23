//! Durable accepted steering and explicit abort behavior across reconstruction.
//! The parent run-loop suite shares its scripted tools and journal fixture.
//! A stopped run must not resurrect queued or already-accepted work.

use super::{Harness, tool_reply};
use roost_agent::{AgentStore, Entry};
use std::time::Duration;

#[tokio::test]
async fn accepted_steers_are_durable_and_an_abort_stays_stopped_after_restart() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .tools
        .hold_next
        .store(true, std::sync::atomic::Ordering::SeqCst);
    harness.llm.script(
        &id,
        tool_reply(&[("call-1", "bash", r#"{"command":"sleep 30"}"#)]),
    );
    harness.submit(&id, "initial work").await;
    for _ in 0..100 {
        if !harness.tools.calls_to("bash").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(harness.tools.calls_to("bash").len(), 1);
    harness.submit(&id, "accepted while the tool runs").await;

    let entries = harness.runtime_entries(&id).await;
    assert_eq!(
        entries
            .iter()
            .filter_map(|(_, entry)| match entry {
                Entry::User { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec!["initial work", "accepted while the tool runs"]
    );
    assert!(matches!(
        entries.iter().rev().find_map(|(_, entry)| match entry {
            Entry::RunIntent { active, .. } => Some(*active),
            _ => None,
        }),
        Some(true)
    ));

    assert!(harness.runtime.abort(&id).await.unwrap());
    let after_abort = harness.runtime_entries(&id).await;
    assert!(matches!(
        after_abort.iter().rev().find_map(|(_, entry)| match entry {
            Entry::RunIntent {
                active, aborted, ..
            } => Some((*active, *aborted)),
            _ => None,
        }),
        Some((false, true))
    ));
    harness.settle(&id).await;

    let restarted = harness.restart_runtime();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 0);
    assert_eq!(harness.llm.requests_for(&id).len(), 1);
}

#[tokio::test]
async fn explicit_input_after_stop_restarts_a_completed_run_once() {
    let (harness, storage_gate) = Harness::gated();
    let id = harness.conversation().await;
    let stream_gate = harness.llm.hold_stream(&id, 1);
    let second_stream_gate = harness.llm.hold_stream(&id, 2);
    harness
        .llm
        .script(&id, super::support::text_reply("first answer"));
    harness
        .llm
        .script(&id, super::support::text_reply("new input answer"));
    storage_gate.hold_completion_admission();
    harness.submit(&id, "first input").await;
    stream_gate.entered.notified().await;
    stream_gate.released.notify_one();
    tokio::time::timeout(
        Duration::from_secs(5),
        storage_gate.completion_admission_held(),
    )
    .await
    .expect("completed run reaches the fenced completion admission");

    harness
        .store
        .admit_command(
            &id,
            roost_agent::CommandAdmission {
                request_id: "stop-before-settle".into(),
                kind: "abort".into(),
                entries: Vec::new(),
                intent: roost_agent::IntentChange::Abort,
                fence: None,
                expected_intent_seq: None,
                completion_through: None,
            },
        )
        .await
        .unwrap();
    harness
        .store
        .admit_command(
            &id,
            roost_agent::CommandAdmission {
                request_id: "explicit-after-stop".into(),
                kind: "submit".into(),
                entries: vec![Entry::User {
                    text: "new input".into(),
                    loop_repeat: false,
                    item_id: Some("explicit-after-stop".into()),
                }],
                intent: roost_agent::IntentChange::Activate,
                fence: None,
                expected_intent_seq: None,
                completion_through: None,
            },
        )
        .await
        .unwrap();
    storage_gate.release_completion_admission();
    tokio::time::timeout(
        Duration::from_secs(5),
        second_stream_gate.entered.notified(),
    )
    .await
    .expect("explicit input after Stop starts a second model call");
    second_stream_gate.released.notify_one();
    let transcript = harness.settle(&id).await;
    assert_eq!(harness.llm.requests_for(&id).len(), 2);
    assert!(transcript.items.iter().any(|item| matches!(item,
        roost_protocol::wire::agent_chat::TranscriptItem::Assistant { blocks, .. }
            if blocks.iter().any(|block| matches!(block,
                roost_protocol::wire::agent_chat::TranscriptBlock::Text { text } if text == "new input answer")
            )
    )));
}
