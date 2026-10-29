//! Durable activation of queued follow-ups across a completion boundary.
//! The parent steering suite supplies the transcript shape projection.
//! This stays separate to keep the root integration module below the file cap.

use roost_agent::records::{Entry, QueuedInputState};
use roost_agent::traits::{
    AdmitOutcome, AgentStore, CommandAdmission, IntentChange, LeaseFence, PlanAdmission,
};

use super::super::support::{Harness, text_reply};

#[tokio::test]
async fn activated_follow_up_survives_completion_before_next_run_start() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let epoch = harness
        .store
        .acquire_lease(&id, harness.runtime.instance_id(), 60_000)
        .await
        .unwrap()
        .unwrap();
    let fence = LeaseFence {
        owner: harness.runtime.instance_id().to_owned(),
        epoch,
    };
    let AdmitOutcome::Admitted { seqs: started, .. } = harness
        .store
        .admit_command(
            &id,
            CommandAdmission {
                request_id: "first".into(),
                kind: "submit".into(),
                entries: vec![Entry::User {
                    text: "first".into(),
                    attachments: Vec::new(),
                    loop_repeat: false,
                    item_id: Some("first".into()),
                }],
                intent: IntentChange::Activate,
                plan_admission: PlanAdmission::AllowPending,
                fence: None,
                expected_intent_seq: None,
                completion_through: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("initial run is admitted");
    };
    harness
        .store
        .append_entry(
            &id,
            &Entry::InputConsumed {
                through_seq: started[0],
            },
            &[],
            Some(&fence),
        )
        .await
        .unwrap();
    harness
        .store
        .admit_command(
            &id,
            CommandAdmission {
                request_id: "follow-up".into(),
                kind: "submit".into(),
                entries: vec![Entry::QueuedInput {
                    submission_id: "follow-up".into(),
                    text: "after completion".into(),
                    attachments: Vec::new(),
                    state: QueuedInputState::Pending,
                }],
                intent: IntentChange::FollowUp,
                plan_admission: PlanAdmission::AllowPending,
                fence: None,
                expected_intent_seq: None,
                completion_through: None,
            },
        )
        .await
        .unwrap();
    harness
        .store
        .admit_command(
            &id,
            CommandAdmission {
                request_id: "complete".into(),
                kind: "run-complete".into(),
                entries: Vec::new(),
                intent: IntentChange::Complete,
                plan_admission: PlanAdmission::AllowPending,
                fence: Some(fence),
                expected_intent_seq: started.last().copied(),
                completion_through: started.first().copied(),
            },
        )
        .await
        .unwrap();

    let pending = harness.runtime.transcript(&id).await.unwrap();
    assert_eq!(pending.queued.len(), 1);
    assert_eq!(pending.queued[0].id, "follow-up");
    harness.llm.script(&id, text_reply("recovered answer"));
    let restarted = harness.restart_runtime();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 1);
    let recovered = harness.settle_runtime(&restarted, &id).await;
    assert_eq!(
        super::shape(&recovered),
        vec![
            "user: first",
            "user: after completion",
            "assistant: recovered answer"
        ]
    );
    assert!(recovered.queued.is_empty());
}
