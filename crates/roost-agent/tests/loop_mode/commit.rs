//! Repeat budget accounting when a store commit wins the race with Stop.
//! The store parks after writing but before acknowledging the entry, so the
//! resumed loop must count that interrupted repeat without issuing its model call.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use roost_agent::{AgentStore, ChatSink, Entry, RuntimeConfig};

use super::support::{Harness, text_reply, tool_reply, user_texts};
use super::{LIMIT_REACHED, loop_status, notice_bodies, parked};

#[path = "commit/store_failures.rs"]
mod store_failures;
use store_failures::FailAppendStore;

#[tokio::test]
async fn stop_after_repeat_commit_spends_that_started_run() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    gate.hold_committed_user_append(2);
    harness.llm.script(&id, text_reply("first"));
    harness.llm.script(&id, text_reply("resumed"));
    harness.submit(&id, "/loop 3 bump").await;
    parked(gate.user_append_held()).await;
    assert!(harness.runtime.abort(&id).await.unwrap());
    gate.release_user_append();
    let paused = harness.settle(&id).await;
    assert!(loop_status(&paused.items).is_some_and(|status| status.contains("paused")));
    assert_eq!(harness.llm.requests_for(&id).len(), 1);

    harness.submit(&id, "again").await;
    let transcript = harness.settle(&id).await;
    let requests = harness.llm.requests_for(&id);
    assert_eq!(
        requests.len(),
        2,
        "the committed repeat already spent the second run"
    );
    assert_eq!(user_texts(&requests[1]), vec!["bump", "bump", "again"]);
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| body == LIMIT_REACHED)
    );
    assert!(loop_status(&transcript.items).is_none());
}

#[tokio::test]
async fn reconfiguring_after_commit_does_not_spend_the_new_loops_budget() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    gate.hold_committed_user_append(2);
    for reply in ["first", "committed", "new first", "new second"] {
        harness.llm.script(&id, text_reply(reply));
    }
    harness.submit(&id, "/loop 3 bump").await;
    parked(gate.user_append_held()).await;
    harness.submit(&id, "/loop 2").await;
    gate.release_user_append();
    let transcript = harness.settle(&id).await;
    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 4);
    assert_eq!(
        user_texts(&requests[3]),
        vec!["bump", "bump", "bump", "bump"]
    );
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| body == LIMIT_REACHED)
    );
    assert!(loop_status(&transcript.items).is_none());
}

#[tokio::test]
async fn an_unappended_prompt_does_not_change_the_loop_body_or_budget() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    harness.submit(&id, "/loop 3").await;
    harness.llm.script(
        &id,
        tool_reply(&[("hold", "bash", r#"{"command":"make"}"#)]),
    );
    harness
        .tools
        .hold_next
        .store(true, std::sync::atomic::Ordering::SeqCst);
    gate.hold_user_append(1);
    let submit = {
        let runtime = harness.runtime.clone();
        let id = id.clone();
        tokio::spawn(async move { runtime.submit(&id, "new body".to_owned()).await })
    };
    parked(gate.user_append_held()).await;

    let transcript = harness.runtime.transcript(&id).await.unwrap();
    assert!(loop_status(&transcript.items).is_some_and(|status| {
        status.contains("waiting for a prompt") && status.contains("3 runs left")
    }));
    let snapshot = harness
        .store
        .entries(&id)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|(_, entry)| match entry {
            roost_agent::Entry::LoopState {
                state_json: Some(json),
            } => Some(json),
            _ => None,
        })
        .unwrap();
    let snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(snapshot["body"], serde_json::Value::Null);
    assert_eq!(snapshot["budget"]["remaining"], 3);
    assert_eq!(snapshot["pending_capture"]["body"], "new body");

    gate.release_user_append();
    submit.await.unwrap().unwrap();
    let transcript = harness.runtime.transcript(&id).await.unwrap();
    assert!(loop_status(&transcript.items).is_some_and(|status| status.contains("2 runs left")));
    assert!(harness.runtime.abort(&id).await.unwrap());
    harness.settle(&id).await;
}

#[tokio::test]
async fn a_failed_user_append_does_not_commit_loop_capture() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let store = Arc::new(FailAppendStore {
        inner: harness.store.clone(),
        fail_next: AtomicBool::new(true),
        fail_active: false,
        intent_failures: AtomicUsize::new(0),
        repeat_failures: AtomicUsize::new(0),
    });
    let runtime = roost_agent::AgentRuntime::new(
        store,
        harness.tools.clone(),
        harness.sink.clone() as Arc<dyn ChatSink>,
        harness.llm.clone(),
        RuntimeConfig {
            retry_backoff: vec![std::time::Duration::from_millis(1); 3],
            advisor_backoff: vec![std::time::Duration::from_millis(1); 3],
            find_timeout: std::time::Duration::from_secs(5),
        },
    );
    runtime.submit(&id, "/loop 3".to_owned()).await.unwrap();
    assert!(runtime.submit(&id, "unaccepted".to_owned()).await.is_err());

    let transcript = runtime.transcript(&id).await.unwrap();
    assert!(loop_status(&transcript.items).is_some_and(|status| {
        status.contains("waiting for a prompt") && status.contains("3 runs left")
    }));
    let snapshot = harness
        .store
        .entries(&id)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|(_, entry)| match entry {
            Entry::LoopState {
                state_json: Some(json),
            } => Some(json),
            _ => None,
        })
        .unwrap();
    let snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(snapshot["body"], serde_json::Value::Null);
    assert_eq!(snapshot["budget"]["remaining"], 3);
    assert_eq!(snapshot["pending_capture"], serde_json::Value::Null);
    assert!(
        !harness
            .store
            .entries(&id)
            .await
            .unwrap()
            .iter()
            .any(|(_, entry)| matches!(entry, Entry::User { text, .. } if text == "unaccepted"))
    );
}

#[tokio::test]
async fn accepted_work_is_readmitted_after_start_or_settlement_store_outages() {
    for fail_active in [true, false] {
        let harness = Harness::new();
        let id = harness.conversation().await;
        let store = Arc::new(FailAppendStore {
            inner: harness.store.clone(),
            fail_next: AtomicBool::new(false),
            fail_active,
            intent_failures: AtomicUsize::new(2),
            repeat_failures: AtomicUsize::new(0),
        });
        let runtime = roost_agent::AgentRuntime::new(
            store.clone(),
            harness.tools.clone(),
            harness.sink.clone(),
            harness.llm.clone(),
            RuntimeConfig {
                retry_backoff: vec![std::time::Duration::from_millis(1)],
                ..RuntimeConfig::default()
            },
        );
        harness
            .llm
            .script(&id, text_reply("The accepted task finished."));
        let submitted = runtime.submit(&id, "Finish the accepted task".into()).await;
        assert_eq!(submitted.is_err(), fail_active);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let entries = store.entries(&id).await.unwrap();
                let terminal = entries
                    .iter()
                    .rev()
                    .find_map(|(_, entry)| match entry {
                        Entry::RunIntent { active, .. } => Some(!active),
                        _ => None,
                    })
                    .unwrap_or(false);
                let transcript = runtime.transcript(&id).await.unwrap();
                if terminal
                    && transcript.run_state == roost_protocol::wire::agent_chat::AgentRunState::Idle
                {
                    assert!(
                        super::support::texts(&transcript)
                            .iter()
                            .any(|text| text == "The accepted task finished.")
                    );
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let requests = harness.llm.requests_for(&id);
        assert_eq!(
            requests.len(),
            1,
            "a saved final response must not be requested twice"
        );
        assert_eq!(user_texts(&requests[0]), vec!["Finish the accepted task"]);
        assert_eq!(store.intent_failures.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn repeat_storage_outages_retry_without_repeating_completed_model_work() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let store = Arc::new(FailAppendStore {
        inner: harness.store.clone(),
        fail_next: AtomicBool::new(false),
        fail_active: false,
        intent_failures: AtomicUsize::new(0),
        repeat_failures: AtomicUsize::new(2),
    });
    let runtime = roost_agent::AgentRuntime::new(
        store.clone(),
        harness.tools.clone(),
        harness.sink.clone(),
        harness.llm.clone(),
        RuntimeConfig {
            retry_backoff: vec![std::time::Duration::from_millis(1)],
            ..RuntimeConfig::default()
        },
    );
    harness
        .llm
        .script(&id, text_reply("First iteration finished."));
    harness
        .llm
        .script(&id, text_reply("Last iteration finished."));
    runtime.submit(&id, "/loop 2 bump".into()).await.unwrap();
    let transcript = harness.settle_runtime(&runtime, &id).await;
    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 2);
    assert_eq!(user_texts(&requests[1]), vec!["bump", "bump"]);
    assert!(loop_status(&transcript.items).is_none());
    assert_eq!(store.repeat_failures.load(Ordering::SeqCst), 0);
}
