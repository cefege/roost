//! Crash recovery preserves admitted input newer than the consumed boundary.
//! The parent module supplies the shared runtime harness.

use roost_agent::AgentStore;
use roost_agent::records::{Entry, PlanState};
use roost_protocol::wire::agent_chat::AgentRunState;

use super::super::support::{Harness, text_reply, user_texts};

#[tokio::test]
async fn reject_after_crash_resumes_only_the_late_unconsumed_steer() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    for entry in [
        Entry::User {
            text: "already consumed".into(),
            attachments: Vec::new(),
            loop_repeat: false,
            item_id: Some("first".into()),
        },
        Entry::InputConsumed { through_seq: 1 },
        Entry::User {
            text: "late steer".into(),
            attachments: Vec::new(),
            loop_repeat: false,
            item_id: Some("late".into()),
        },
        Entry::RunIntent {
            active: true,
            through_seq: 3,
            aborted: false,
            yielded: false,
        },
        Entry::PlanProposal {
            item_id: "plan".into(),
            title: "Pending plan".into(),
            content: "1. decide first".into(),
            state: PlanState::Proposed,
        },
    ] {
        harness
            .store
            .append_entry(&id, &entry, &[], None)
            .await
            .unwrap();
    }
    let mut record = harness.store.conversation(&id).await.unwrap().unwrap();
    record.run_state = AgentRunState::Running;
    let revision = record.revision;
    harness
        .store
        .save_conversation(&record, revision)
        .await
        .unwrap();

    let restarted = harness.restart_runtime();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 0);
    harness.llm.script(&id, text_reply("resumed"));
    restarted
        .plan_decide(&id, "plan", "reject", "")
        .await
        .unwrap();
    harness.settle_runtime(&restarted, &id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 1);
    assert!(
        user_texts(&requests[0])
            .iter()
            .any(|text| text == "late steer")
    );
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 0);
}
