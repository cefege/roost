//! A tool round journals completed effects before dispatching its next call.
//! A blocked second call leaves the first result recoverable from the store,
//! rather than losing the whole round if the coordinator is killed.

use std::sync::Arc;

use futures::future::BoxFuture;
use roost_agent::{
    AgentRuntime, AgentStore, Entry, RuntimeConfig, ToolCall, ToolExecutor, ToolOutcome,
};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use super::support::{Harness, tool_reply};

#[derive(Default)]
struct BlockingSecondTool {
    second_started: Notify,
}

impl ToolExecutor for BlockingSecondTool {
    fn execute<'a>(
        &'a self,
        _worker_fp: &'a str,
        call: ToolCall,
        _out: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolOutcome, String>> {
        Box::pin(async move {
            if call.call_id == "second-effect" {
                self.second_started.notify_one();
                cancel.cancelled().await;
                return Err("cancelled".into());
            }
            let content = if call.tool == "context_files" {
                r#"{"context":"","watchdog":""}"#
            } else {
                "First effect finished; checkpoint is durable."
            };
            Ok(ToolOutcome {
                content: content.into(),
                ..ToolOutcome::default()
            })
        })
    }

    fn close_conversation<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

#[tokio::test]
async fn a_completed_effect_is_saved_before_the_next_effect_finishes() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let tools = Arc::new(BlockingSecondTool::default());
    let runtime = AgentRuntime::new(
        harness.store.clone(),
        tools.clone(),
        harness.sink.clone(),
        harness.llm.clone(),
        RuntimeConfig::default(),
    );
    harness.llm.script(
        &id,
        tool_reply(&[
            ("first-effect", "bash", r#"{"command":"first-action"}"#),
            ("second-effect", "bash", r#"{"command":"second-action"}"#),
        ]),
    );
    runtime
        .submit(&id, "Perform both actions".into())
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tools.second_started.notified(),
    )
    .await
    .unwrap();

    let entries = harness.store.entries(&id).await.unwrap();
    assert!(entries.iter().any(|(_, entry)| matches!(entry,
        Entry::ToolResult { call_id, text, is_error: false, .. }
            if call_id == "first-effect" && text == "First effect finished; checkpoint is durable.")));
    assert!(!entries.iter().any(|(_, entry)| matches!(entry,
        Entry::ToolResult { call_id, .. } if call_id == "second-effect")));
    runtime.abort(&id).await.unwrap();
}

#[tokio::test]
async fn provider_backoff_keeps_stop_available_and_cancellation_prevents_retry() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let runtime = AgentRuntime::new(
        harness.store.clone(),
        harness.tools.clone(),
        harness.sink.clone(),
        harness.llm.clone(),
        RuntimeConfig {
            retry_backoff: vec![std::time::Duration::from_secs(60)],
            ..RuntimeConfig::default()
        },
    );
    harness.llm.script(
        &id,
        Err(roost_llm::LlmError::Http {
            status: 400,
            body: "provider rejected the request".into(),
        }),
    );
    runtime.submit(&id, "Keep working".into()).await.unwrap();
    let failed_attempt = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let transcript = runtime.transcript(&id).await.unwrap();
            if transcript.error.is_some() {
                break transcript;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        failed_attempt.run_state,
        roost_protocol::wire::agent_chat::AgentRunState::Running
    );
    assert!(runtime.abort(&id).await.unwrap());
    let settled = harness.settle_runtime(&runtime, &id).await;
    assert_eq!(
        settled.run_state,
        roost_protocol::wire::agent_chat::AgentRunState::Idle
    );
    assert_eq!(harness.llm.requests_for(&id).len(), 1);
    assert_eq!(runtime.recover_interrupted_runs().await.unwrap(), 0);
}

#[tokio::test]
async fn queued_effects_are_distinct_from_execution_and_abort_does_not_dispatch_them() {
    use roost_protocol::wire::agent_chat::{TranscriptItem, fold_chat_event};
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    let harness = Harness::new();
    let id = harness.conversation().await;
    let mut live = harness.runtime.transcript(&id).await.unwrap();
    harness.tools.hold_next.store(true, Ordering::SeqCst);
    harness.llm.script(
        &id,
        tool_reply(&[
            ("running-effect", "bash", r#"{"command":"first-action"}"#),
            (
                "queued-effect",
                "write",
                r#"{"path":"second-effect","content":"must not write"}"#,
            ),
        ]),
    );
    harness.submit(&id, "Perform both actions").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while harness.tools.calls_to("bash").is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for event in harness.sink.events_for(&id) {
        fold_chat_event(&mut live, &event);
    }
    assert!(live.items.iter().any(|item| matches!(item,
        TranscriptItem::Tool { call_id, running: true, queued: false, .. }
        if call_id == "running-effect")));
    assert!(live.items.iter().any(|item| matches!(item,
        TranscriptItem::Tool { call_id, running: true, queued: true, .. }
        if call_id == "queued-effect")));

    harness.runtime.abort(&id).await.unwrap();
    let settled = harness.settle(&id).await;
    assert!(harness.tools.calls_to("write").is_empty());
    let details = settled
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::Tool {
                call_id,
                details_json,
                is_error: false,
                ..
            } if call_id == "queued-effect" => Some(details_json),
            _ => None,
        })
        .expect("cancelled queued effect");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(details).unwrap()["cancelled"],
        true
    );
}
