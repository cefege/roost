//! Accepted-input effects must precede the runner's first model request.
//! The parent module provides the shared runtime harness.
//! A paused first stream keeps goal continuation from racing the assertions.

use roost_agent::records::AdvisorySeverity;
use roost_agent::{AgentStore, Entry};
use roost_protocol::wire::agent_chat::{GoalState, GoalStatus, TranscriptItem};

use super::super::support::{Harness, text_reply, user_texts};

#[tokio::test]
async fn accepted_input_delivers_advisory_and_rearms_waiting_goal_before_first_request() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let goal = GoalState {
        id: "goal".into(),
        objective: "Finish the task".into(),
        status: GoalStatus::Active,
        token_budget: None,
        tokens_used: 0,
        time_used_seconds: 0,
        created_ms: 1,
        updated_ms: 1,
        report: None,
        waiting_reason: Some("waiting for user input".into()),
    };
    for entry in [
        Entry::Goal { state: goal },
        Entry::Advisory {
            item_id: "advisory".into(),
            severity: AdvisorySeverity::Concern,
            note: "include this advisory".into(),
            delivered: false,
        },
    ] {
        harness
            .store
            .append_entry(&id, &entry, &[], None)
            .await
            .unwrap();
    }
    harness.llm.script(&id, text_reply("handled"));
    let stream_gate = harness.llm.hold_stream(&id, 1);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        harness
            .runtime
            .submit_request(&id, "input", "continue work".into()),
    )
    .await
    .expect("submission does not wait for the held model stream")
    .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream_gate.entered.notified(),
    )
    .await
    .expect("the first model request reaches the stream gate");

    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 1);
    assert!(
        user_texts(&requests[0])
            .iter()
            .any(|text| text.contains("include this advisory")),
        "the runner's first request includes the advisory"
    );
    let transcript = harness.runtime.transcript(&id).await.unwrap();
    let goal = transcript
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::Goal { state, .. } => Some(state),
            _ => None,
        })
        .unwrap();
    assert!(goal.waiting_reason.is_none());
    stream_gate.released.notify_one();
    harness.settle(&id).await;
}
