//! Behaviour tests for background jobs inside the agent loop: `wait` with nothing
//! running, and a settled job delivered into the next model request.

#[path = "support/mod.rs"]
mod support;

use roost_agent::Entry;
use roost_protocol::wire::agent_chat::JobState;
use support::{Harness, text_reply, tool_reply, user_texts};

#[tokio::test]
async fn wait_with_no_running_job_reports_nothing_to_wait_for() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .llm
        .script(&id, tool_reply(&[("call-1", "wait", "{}")]));
    harness.llm.script(&id, text_reply("done waiting"));
    harness.submit(&id, "wait for the build").await;
    harness.settle(&id).await;
    let requests = harness.llm.requests_for(&id);
    let follow_up = requests
        .last()
        .expect("the model is asked again after wait");
    let answered = follow_up.messages.iter().any(|message| {
        matches!(message, roost_llm::Message::ToolResult { call_id, text, .. }
            if call_id == "call-1" && text.contains("Nothing to wait for"))
    });
    assert!(answered, "wait must answer at once when no job runs");
}

#[tokio::test]
async fn settled_job_is_delivered_into_the_next_model_request() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(&id, text_reply("first answer"));
    harness.submit(&id, "start the build").await;
    harness.settle(&id).await;

    harness.llm.script(&id, text_reply("saw the build result"));
    harness
        .runtime
        .deliver_background_job(
            &id,
            Entry::BackgroundJob {
                job_id: "bg_7".into(),
                command: "make".into(),
                state: JobState::Completed,
                is_error: false,
                output: "build ok".into(),
                exit_code: Some(0),
            },
        )
        .await
        .expect("delivery is accepted");
    harness.settle(&id).await;

    let requests = harness.llm.requests_for(&id);
    let last = requests.last().expect("a request follows the delivery");
    let delivered = user_texts(last).join("\n");
    assert!(delivered.contains("<system-notice>"), "{delivered}");
    assert!(delivered.contains("bg_7"), "{delivered}");
    assert!(delivered.contains("build ok"), "{delivered}");
}
