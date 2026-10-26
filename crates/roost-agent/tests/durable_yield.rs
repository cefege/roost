//! A confirmed tool handoff must survive loss of its callback's process state.
//! The store gate pauses after SQL-equivalent settlement, before kernel adoption;
//! a second owner hydrates that receipt and settles the same child conversation.

mod support;

use std::time::Duration;

use roost_agent::{AgentRuntime, AgentStore, Entry, RuntimeConfig};
use support::{Harness, tool_reply};

#[tokio::test]
async fn owner_handover_after_yield_receipt_keeps_report_without_another_model_call() {
    let (harness, gate) = Harness::gated();
    let parent_id = harness.conversation().await;
    let child_id = harness.conversation().await;
    let mut child = harness
        .store
        .conversation(&child_id)
        .await
        .unwrap()
        .unwrap();
    child.parent_id = Some(parent_id);
    child.agent = Some("task".into());
    harness
        .store
        .save_conversation(&child, child.revision)
        .await
        .unwrap();
    gate.hold_next_yield_receipt();
    harness.llm.script(
        &child_id,
        tool_reply(&[(
            "yield-report",
            "yield",
            r#"{"result":"durably completed child report"}"#,
        )]),
    );
    harness
        .submit(&child_id, "Complete this child task and yield the report.")
        .await;
    let held = tokio::time::timeout(Duration::from_secs(5), gate.yield_receipt_held())
        .await
        .expect("the yield outcome is confirmed before generation consumes it");
    assert_eq!(held, child_id);
    assert_eq!(harness.runtime.release_all_leases().await, 1);

    let successor = AgentRuntime::new(
        harness.store.clone(),
        harness.tools.clone(),
        harness.sink.clone(),
        harness.llm.clone(),
        RuntimeConfig {
            instance_id: "yield-successor".into(),
            retry_backoff: vec![Duration::from_millis(1); 3],
            ..RuntimeConfig::default()
        },
    );
    assert_eq!(successor.recover_interrupted_runs().await.unwrap(), 1);
    harness.settle_runtime(&successor, &child_id).await;
    gate.release_yield_receipt();

    let entries = harness.store.entries(&child_id).await.unwrap();
    assert!(matches!(
        entries.iter().rev().find_map(|(_, entry)| match entry {
            Entry::RunIntent {
                active,
                aborted,
                yielded,
                ..
            } => Some((*active, *aborted, *yielded)),
            _ => None,
        }),
        Some((false, false, true))
    ));
    assert_eq!(
        harness.llm.requests_for(&child_id).len(),
        1,
        "the successor consumes the saved yield rather than asking the model to repeat it"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|(_, entry)| matches!(entry,
                Entry::ToolResult { call_id, is_error: false, .. } if call_id == "yield-report"
            ))
            .count(),
        1
    );
}
