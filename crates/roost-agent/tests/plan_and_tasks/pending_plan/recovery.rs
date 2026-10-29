//! Pending plans suppress crash recovery until the user decides them.
//! Rejecting a proposal resumes accepted held input without creating an empty run.
//! The parent module supplies the in-memory runtime harness and model scripts.

use roost_agent::AgentStore;
use roost_agent::records::{Entry, PlanState, QueuedInputState};
use roost_protocol::wire::agent_chat::AgentRunState;

use super::super::support::{Harness, text_reply, user_texts};

#[tokio::test]
async fn restart_does_not_recover_an_active_run_after_a_plan_is_proposed() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .store
        .append_entry(
            &id,
            &Entry::User {
                text: "accepted before the plan".into(),
                attachments: Vec::new(),
                loop_repeat: false,
                item_id: Some("accepted".into()),
            },
            &[],
            None,
        )
        .await
        .unwrap();
    harness
        .store
        .append_entry(
            &id,
            &Entry::RunIntent {
                active: true,
                through_seq: 1,
                aborted: false,
                yielded: false,
            },
            &[],
            None,
        )
        .await
        .unwrap();
    harness
        .store
        .append_entry(&id, &Entry::InputConsumed { through_seq: 1 }, &[], None)
        .await
        .unwrap();
    let advisory_seq = harness
        .store
        .append_entry(
            &id,
            &Entry::Advisory {
                item_id: "consumed-advisory".into(),
                severity: roost_agent::records::AdvisorySeverity::Concern,
                note: "already consumed".into(),
                delivered: true,
            },
            &[],
            None,
        )
        .await
        .unwrap();
    harness
        .store
        .append_entry(
            &id,
            &Entry::InputConsumed {
                through_seq: advisory_seq,
            },
            &[],
            None,
        )
        .await
        .unwrap();
    harness
        .store
        .append_entry(
            &id,
            &Entry::PlanProposal {
                item_id: "plan".into(),
                title: "Pending plan".into(),
                content: "1. decide first".into(),
                state: PlanState::Proposed,
            },
            &[],
            None,
        )
        .await
        .unwrap();

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
    assert_eq!(
        restarted.conversation(&id).await.unwrap().run_state,
        AgentRunState::Idle
    );
    restarted
        .plan_decide(&id, "plan", "reject", "")
        .await
        .unwrap();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 0);
    assert!(harness.llm.requests_for(&id).is_empty());
}

#[tokio::test]
async fn reject_resumes_held_and_queued_inputs_without_a_phantom_run() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    for entry in [
        Entry::User {
            text: "held steer".into(),
            attachments: Vec::new(),
            loop_repeat: false,
            item_id: Some("held".into()),
        },
        Entry::RunIntent {
            active: false,
            through_seq: 0,
            aborted: false,
            yielded: false,
        },
        Entry::QueuedInput {
            submission_id: "queued".into(),
            text: "queued draft".into(),
            attachments: Vec::new(),
            state: QueuedInputState::Pending,
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
    harness.llm.script(&id, text_reply("resumed"));

    harness
        .runtime
        .plan_decide(&id, "plan", "reject", "")
        .await
        .unwrap();
    harness.settle(&id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 1);
    assert_eq!(user_texts(&requests[0]), vec!["held steer", "queued draft"]);
}
