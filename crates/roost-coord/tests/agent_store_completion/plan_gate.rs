//! SQL-store regression coverage for pending-plan command admission.
//! These tests exercise the same transaction path used by coordinator replicas.
//! They pin rejection, single-shot resolution, and queued-input preservation.

use roost_agent::records::{Entry, PlanState, QueuedInputState};
use roost_agent::traits::{
    AdmitOutcome, AgentStore, CommandAdmission, IntentChange, LeaseFence, PlanAdmission,
};

use super::CoordAgentStore;

#[tokio::test]
async fn pending_plan_refusal_does_not_reserve_a_sql_submission_or_decision() {
    let root = super::scratch_root();
    let db = super::db_support::open_test_database(&root).await.unwrap();
    let store = CoordAgentStore::new(db.clone());
    store
        .save_conversation(&super::conversation("plan"), 0)
        .await
        .unwrap();
    store
        .admit_command(
            "plan",
            CommandAdmission {
                request_id: "proposal".into(),
                kind: "proposal".into(),
                entries: vec![Entry::PlanProposal {
                    item_id: "plan-item".into(),
                    title: "Plan".into(),
                    content: "1. decide".into(),
                    state: PlanState::Proposed,
                }],
                intent: IntentChange::None,
                plan_admission: PlanAdmission::AllowPending,
                fence: None,
                expected_intent_seq: None,
                completion_through: None,
            },
        )
        .await
        .unwrap();
    let submission = CommandAdmission {
        request_id: "submit-after-plan".into(),
        kind: "submit".into(),
        entries: vec![Entry::User {
            text: "blocked until decision".into(),
            attachments: Vec::new(),
            loop_repeat: false,
            item_id: Some("submit-after-plan".into()),
        }],
        intent: IntentChange::Activate,
        plan_admission: PlanAdmission::RejectIfPending,
        fence: None,
        expected_intent_seq: None,
        completion_through: None,
    };
    assert_eq!(
        store
            .admit_command("plan", submission.clone())
            .await
            .unwrap(),
        AdmitOutcome::PendingPlan
    );
    let decision = CommandAdmission {
        request_id: "resolve".into(),
        kind: "plan-resolve".into(),
        entries: vec![Entry::PlanProposal {
            item_id: "plan-item".into(),
            title: "Plan".into(),
            content: "1. decide".into(),
            state: PlanState::Rejected,
        }],
        intent: IntentChange::None,
        plan_admission: PlanAdmission::RequireProposed {
            item_id: "plan-item".into(),
        },
        fence: None,
        expected_intent_seq: None,
        completion_through: None,
    };
    assert!(matches!(
        store.admit_command("plan", decision.clone()).await.unwrap(),
        AdmitOutcome::Admitted { .. }
    ));
    assert_eq!(
        store.admit_command("plan", decision.clone()).await.unwrap(),
        AdmitOutcome::Duplicate { result_json: None }
    );
    let mut competing_decision = decision;
    competing_decision.request_id = "resolve-again".into();
    assert_eq!(
        store
            .admit_command("plan", competing_decision)
            .await
            .unwrap(),
        AdmitOutcome::PlanNotProposed
    );
    assert!(matches!(
        store.admit_command("plan", submission).await.unwrap(),
        AdmitOutcome::Admitted { .. }
    ));
    db.pool().close().await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn completion_holds_sql_queued_input_while_plan_is_pending() {
    let root = super::scratch_root();
    let db = super::db_support::open_test_database(&root).await.unwrap();
    let store = CoordAgentStore::new(db.clone());
    store
        .save_conversation(&super::conversation("queue"), 0)
        .await
        .unwrap();
    let epoch = store
        .acquire_lease("queue", "coord", 60_000)
        .await
        .unwrap()
        .unwrap();
    let fence = LeaseFence {
        owner: "coord".into(),
        epoch,
    };
    let AdmitOutcome::Admitted { seqs: started, .. } = store
        .admit_command(
            "queue",
            CommandAdmission {
                request_id: "start".into(),
                kind: "submit".into(),
                entries: vec![Entry::User {
                    text: "current".into(),
                    attachments: Vec::new(),
                    loop_repeat: false,
                    item_id: Some("start".into()),
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
    store
        .admit_command(
            "queue",
            CommandAdmission {
                request_id: "pending".into(),
                kind: "submit".into(),
                entries: vec![
                    Entry::QueuedInput {
                        submission_id: "queued".into(),
                        text: "keep draft".into(),
                        attachments: Vec::new(),
                        state: QueuedInputState::Pending,
                    },
                    Entry::PlanProposal {
                        item_id: "plan-item".into(),
                        title: "Plan".into(),
                        content: "1. decide".into(),
                        state: PlanState::Proposed,
                    },
                ],
                intent: IntentChange::None,
                plan_admission: PlanAdmission::AllowPending,
                fence: None,
                expected_intent_seq: None,
                completion_through: None,
            },
        )
        .await
        .unwrap();
    store
        .admit_command(
            "queue",
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
    let entries = store.entries("queue").await.unwrap();
    assert!(entries.iter().any(|(_, entry)| matches!(
        entry,
        Entry::QueuedInput {
            state: QueuedInputState::Pending,
            ..
        }
    )));
    assert!(!entries.iter().any(|(_, entry)| matches!(
        entry,
        Entry::QueuedInput {
            state: QueuedInputState::Activated,
            ..
        }
    )));
    assert!(matches!(
        entries.iter().rev().find_map(|(_, entry)| match entry {
            Entry::RunIntent { active, .. } => Some(*active),
            _ => None,
        }),
        Some(false)
    ));
    db.pool().close().await;
    let _ = std::fs::remove_dir_all(root);
}
