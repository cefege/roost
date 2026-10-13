//! Behaviour tests for journal rewind, retry and independent conversation forks.
//! Rewind must affect both the transcript and subsequent provider history.
//! Worker restore calls use the original user-entry sequence as their checkpoint.

#[path = "support/mod.rs"]
mod support;

#[path = "rewind_fork/recovery.rs"]
mod recovery;

use roost_agent::{AgentStore, Entry};
use roost_llm::{AssistantBlock, Message, Usage};
use roost_protocol::wire::agent_chat::{AgentRunState, TranscriptItem};
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

#[tokio::test]
async fn recovery_resumes_current_and_legacy_interrupted_work_once() {
    let harness = Harness::new();
    let interrupted_id = harness.conversation().await;
    let legacy_id = harness.conversation().await;
    let failed_id = harness.conversation().await;
    let mut record = harness
        .store
        .conversation(&interrupted_id)
        .await
        .unwrap()
        .unwrap();
    record.run_state = AgentRunState::Running;
    harness
        .store
        .save_conversation(&record, record.revision)
        .await
        .unwrap();
    let entries = [
        Entry::User {
            text: "prior request".into(),
            loop_repeat: false,
        },
        Entry::Assistant {
            item_id: "prior-tool-call".into(),
            blocks: vec![AssistantBlock::ToolCall {
                call_id: "prior-call".into(),
                name: "read".into(),
                args_json: r#"{"path":"old.txt"}"#.into(),
            }],
            usage: Usage {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
            },
            model: Some(roost_agent::records::model_ref(
                support::PROVIDER,
                support::MODEL,
            )),
            metadata: None,
        },
        Entry::Notice {
            level: "warn".into(),
            title: "Run attempt failed".into(),
            body: "provider disconnected".into(),
        },
        Entry::ToolResult {
            call_id: "prior-call".into(),
            tool_name: "read".into(),
            text: "persisted tool result".into(),
            is_error: false,
            details_json: "{}".into(),
        },
        Entry::Assistant {
            item_id: "prior-answer".into(),
            blocks: vec![AssistantBlock::Text {
                text: "prior result".into(),
            }],
            usage: Usage {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
            },
            model: Some(roost_agent::records::model_ref(
                support::PROVIDER,
                support::MODEL,
            )),
            metadata: None,
        },
        Entry::User {
            text: "unfinished request".into(),
            loop_repeat: false,
        },
        Entry::RunIntent {
            active: true,
            through_seq: 6,
            aborted: false,
        },
    ];
    for entry in entries {
        harness
            .store
            .append_entry(&interrupted_id, &entry, &[], None)
            .await
            .unwrap();
    }

    let legacy_entries = [
        Entry::User {
            text: "legacy unfinished request".into(),
            loop_repeat: false,
        },
        Entry::Assistant {
            item_id: "legacy-tool-call".into(),
            blocks: vec![AssistantBlock::ToolCall {
                call_id: "legacy-call".into(),
                name: "bash".into(),
                args_json: r#"{"command":"make"}"#.into(),
            }],
            usage: Usage {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
            },
            model: Some(roost_agent::records::model_ref(
                support::PROVIDER,
                support::MODEL,
            )),
            metadata: None,
        },
        Entry::Notice {
            level: "warn".into(),
            title: "Run interrupted".into(),
            body: "Run interrupted by a coordinator restart".into(),
        },
    ];
    for entry in legacy_entries {
        harness
            .store
            .append_entry(&legacy_id, &entry, &[], None)
            .await
            .unwrap();
    }
    let mut failed_record = harness
        .store
        .conversation(&failed_id)
        .await
        .unwrap()
        .unwrap();
    failed_record.run_state = AgentRunState::Failed;
    harness
        .store
        .save_conversation(&failed_record, failed_record.revision)
        .await
        .unwrap();
    for entry in [
        Entry::User {
            text: "legacy failed request".into(),
            loop_repeat: false,
        },
        Entry::Assistant {
            item_id: "failed-attempt".into(),
            blocks: vec![AssistantBlock::ToolCall {
                call_id: "failed-prior-call".into(),
                name: "read".into(),
                args_json: r#"{"path":"before-failure.txt"}"#.into(),
            }],
            usage: Usage {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
            },
            model: None,
            metadata: None,
        },
        Entry::ToolResult {
            call_id: "failed-prior-call".into(),
            tool_name: "read".into(),
            text: "persisted before provider failure".into(),
            is_error: false,
            details_json: "{}".into(),
        },
    ] {
        harness
            .store
            .append_entry(&failed_id, &entry, &[], None)
            .await
            .unwrap();
    }
    harness
        .llm
        .script(&interrupted_id, text_reply("recovered answer"));
    harness
        .llm
        .script(&legacy_id, text_reply("legacy recovered answer"));
    harness
        .llm
        .script(&failed_id, text_reply("legacy failed row recovered"));

    let restarted = harness.restart_runtime();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 3);
    harness.settle_runtime(&restarted, &interrupted_id).await;
    harness.settle_runtime(&restarted, &legacy_id).await;
    harness.settle_runtime(&restarted, &failed_id).await;

    let resumed = &harness.llm.requests_for(&interrupted_id)[0];
    assert_eq!(
        user_texts(resumed),
        vec!["prior request", "unfinished request"]
    );
    assert!(resumed.messages.iter().any(|message| matches!(
        message,
        Message::ToolResult { call_id, text, .. }
            if call_id == "prior-call" && text == "persisted tool result"
    )));
    assert!(resumed.messages.iter().any(|message| matches!(
        message,
        Message::Assistant { blocks }
            if blocks.iter().any(|block| matches!(block, AssistantBlock::Text { text } if text == "prior result"))
    )));
    let legacy_request = &harness.llm.requests_for(&legacy_id)[0];
    assert_eq!(
        user_texts(legacy_request),
        vec!["legacy unfinished request"]
    );
    assert!(legacy_request.messages.iter().any(|message| matches!(
        message,
        Message::ToolResult { call_id, is_error: true, .. } if call_id == "legacy-call"
    )));
    let failed_request = &harness.llm.requests_for(&failed_id)[0];
    assert_eq!(user_texts(failed_request), vec!["legacy failed request"]);
    assert!(failed_request.messages.iter().any(|message| matches!(
        message,
        Message::ToolResult { call_id, text, .. }
            if call_id == "failed-prior-call" && text == "persisted before provider failure"
    )));

    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 0);
    assert_eq!(harness.llm.requests_for(&interrupted_id).len(), 1);
    assert_eq!(harness.llm.requests_for(&legacy_id).len(), 1);
    assert_eq!(harness.llm.requests_for(&failed_id).len(), 1);
}
