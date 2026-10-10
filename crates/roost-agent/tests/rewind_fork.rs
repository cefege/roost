//! Behaviour tests for journal rewind, retry and independent conversation forks.
//! Rewind must affect both the transcript and subsequent provider history.
//! Worker restore calls use the original user-entry sequence as their checkpoint.

#[path = "support/mod.rs"]
mod support;

use roost_agent::{AgentStore, Entry};
use roost_protocol::wire::agent_chat::TranscriptItem;
use support::{Harness, text_reply, user_texts};

#[tokio::test]
async fn rewind_hides_turn_and_restores_its_worker_checkpoint() {
    let harness = Harness::new();
    let conversation_id = harness.conversation().await;
    harness
        .llm
        .script(&conversation_id, text_reply("first answer"));
    harness
        .llm
        .script(&conversation_id, text_reply("second answer"));
    harness.submit(&conversation_id, "old turn text").await;
    harness.settle(&conversation_id).await;
    let seq = harness
        .store
        .entries(&conversation_id)
        .await
        .unwrap()
        .into_iter()
        .find_map(|(seq, entry)| matches!(entry, Entry::User { .. }).then_some(seq))
        .expect("user entry");
    harness
        .tools
        .answer("rewind_files", "Restored files:\n/repo/changed.txt");
    let restored_text = harness
        .runtime
        .rewind(&conversation_id, &format!("u{seq}"), true)
        .await
        .unwrap();
    assert_eq!(restored_text, "old turn text");
    let transcript = harness.runtime.transcript(&conversation_id).await.unwrap();
    assert!(!transcript.items.iter().any(|item| matches!(item,
        TranscriptItem::User { text, .. } if text == "old turn text"
    )));
    let restore_call = harness.tools.calls_to("rewind_files");
    assert_eq!(restore_call.len(), 1);
    assert_eq!(restore_call[0].checkpoint_id, seq);
    assert_eq!(
        restore_call[0].args_json,
        format!(r#"{{"checkpoint_id":{seq}}}"#)
    );

    harness.submit(&conversation_id, "new turn text").await;
    harness.settle(&conversation_id).await;
    let requests = harness.llm.requests_for(&conversation_id);
    assert_eq!(user_texts(&requests[1]), vec!["new turn text"]);
}

#[tokio::test]
async fn retry_resubmits_the_same_text_without_restoring_files() {
    let harness = Harness::new();
    let conversation_id = harness.conversation().await;
    harness
        .llm
        .script(&conversation_id, text_reply("first answer"));
    harness
        .llm
        .script(&conversation_id, text_reply("retried answer"));
    harness
        .submit(&conversation_id, "retry this exact text")
        .await;
    harness.settle(&conversation_id).await;
    harness.submit(&conversation_id, "/retry").await;
    harness.settle(&conversation_id).await;
    let requests = harness.llm.requests_for(&conversation_id);
    assert_eq!(requests.len(), 2);
    assert_eq!(user_texts(&requests[0]), vec!["retry this exact text"]);
    assert_eq!(user_texts(&requests[1]), vec!["retry this exact text"]);
    assert!(harness.tools.calls_to("rewind_files").is_empty());
}

#[tokio::test]
async fn fork_copies_history_and_runs_independently() {
    let harness = Harness::new();
    let source_id = harness.conversation().await;
    harness.llm.script(&source_id, text_reply("source answer"));
    harness.submit(&source_id, "shared history").await;
    harness.settle(&source_id).await;

    let fork = harness.runtime.fork(&source_id, None).await.unwrap();
    assert_ne!(fork.id, source_id);
    assert_eq!(fork.title, "shared history (fork)");
    let fork_transcript = harness.runtime.transcript(&fork.id).await.unwrap();
    assert!(fork_transcript.items.iter().any(|item| matches!(item,
        TranscriptItem::User { text, .. } if text == "shared history"
    )));
    harness.llm.script(&fork.id, text_reply("fork-only answer"));
    harness.submit(&fork.id, "continue only in fork").await;
    harness.settle(&fork.id).await;
    assert!(
        harness
            .runtime
            .transcript(&source_id)
            .await
            .unwrap()
            .items
            .iter()
            .all(|item| !matches!(item,
                TranscriptItem::User { text, .. } if text == "continue only in fork"
            ))
    );
    assert_eq!(
        user_texts(&harness.llm.requests_for(&fork.id)[0]),
        vec!["shared history", "continue only in fork"]
    );
}
