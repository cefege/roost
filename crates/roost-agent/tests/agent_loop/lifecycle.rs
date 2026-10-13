//! Durable accepted steering and explicit abort behavior across reconstruction.
//! The parent run-loop suite shares its scripted tools and journal fixture.
//! A stopped run must not resurrect queued or already-accepted work.

use super::support::{Harness, tool_reply};
use roost_agent::Entry;
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
