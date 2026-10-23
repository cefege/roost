//! A fork may omit auxiliary journal rows without consuming its future inputs.
//! Real model dispatch creates the source's consumption boundary before copying.
//! The held fork request leaves a later steer available for durable withdrawal.

use super::support::{Harness, MODEL, PROVIDER, text_reply};
use roost_agent::{AgentStore, Entry};
use roost_llm::Usage;
use std::time::Duration;

#[tokio::test]
async fn forked_steer_remains_withdrawable_after_source_sequence_gaps() {
    let harness = Harness::new();
    let source_id = harness.conversation().await;
    for _ in 0..64 {
        harness
            .store
            .append_entry(
                &source_id,
                &Entry::ExtraUsage {
                    provider: PROVIDER.into(),
                    model_id: MODEL.into(),
                    usage: Usage::default(),
                },
                &[],
                None,
            )
            .await
            .unwrap();
    }
    harness.llm.script(&source_id, text_reply("source answer"));
    harness.submit(&source_id, "source input").await;
    harness.settle(&source_id).await;
    let fork = harness.runtime.fork(&source_id, None).await.unwrap();
    let gate = harness.llm.hold_stream(&fork.id, 1);
    harness.llm.script(&fork.id, text_reply("fork answer"));
    harness
        .runtime
        .submit_request(&fork.id, "fork-first", "first fork input".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("fork reaches its provider request");
    harness
        .runtime
        .submit_request(&fork.id, "fork-steer", "withdraw this fork steer".into())
        .await
        .unwrap();
    let queued = harness.runtime.transcript(&fork.id).await.unwrap().queued;
    assert!(
        queued.iter().any(|message| message.id == "fork-steer"),
        "copied source sequence numbers must not hide a new fork input"
    );
    assert!(
        harness
            .runtime
            .withdraw(&fork.id, "withdraw-fork-steer", "fork-steer")
            .await
            .unwrap()
    );
    gate.released.notify_one();
    let settled = harness.settle(&fork.id).await;
    assert!(settled.queued.is_empty());
    assert!(!settled.items.iter().any(|item| matches!(item,
        roost_protocol::wire::agent_chat::TranscriptItem::User { text, .. }
            if text == "withdraw this fork steer"
    )));
    assert_eq!(harness.llm.requests_for(&fork.id).len(), 1);
}
