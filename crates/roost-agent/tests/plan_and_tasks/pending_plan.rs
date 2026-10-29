//! Pending-plan admission regressions for ordinary messages and queued follow-ups.
//! The runtime rejects input before journal admission while a proposal awaits a decision.
//! The store also keeps already-durable queued drafts outside the next run until resolution.

use roost_agent::records::{Entry, PlanState, QueuedInputState};
use roost_agent::{
    AgentError, AgentStore, CommandAdmission, IntentChange, LeaseFence, PlanAdmission,
};
use roost_protocol::wire::agent_chat::TranscriptItem;

use super::support::{Harness, text_reply, tool_reply, user_texts};
#[path = "pending_plan/admission_effects.rs"]
mod admission_effects;
#[path = "pending_plan/late_steer.rs"]
mod late_steer;

#[path = "pending_plan/recovery.rs"]
mod recovery;

#[tokio::test]
async fn pending_plan_refuses_unadmitted_submit_until_rejected_then_retries() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(
        &id,
        tool_reply(&[(
            "call-plan",
            "propose_plan",
            r#"{"title":"Plan","plan":"1. make the change"}"#,
        )]),
    );
    harness
        .runtime
        .submit_request(&id, "plan-request", "/plan make the change".into())
        .await
        .unwrap();
    let transcript = harness.settle(&id).await;
    let plan_id = transcript
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::Plan { id, state, .. } if state == "proposed" => Some(id.clone()),
            _ => None,
        })
        .expect("a proposed plan card");

    harness
        .runtime
        .submit_request(&id, "plan-request", "/plan make the change".into())
        .await
        .unwrap();

    let blocked = harness
        .runtime
        .submit_request(&id, "retry-after-reject", "ordinary input".into())
        .await;
    assert!(matches!(blocked, Err(AgentError::FailedPrecondition(_))));
    let blocked_transcript = harness.runtime.transcript(&id).await.unwrap();
    assert!(blocked_transcript.items.iter().all(|item| {
        !matches!(item, TranscriptItem::User { text, .. } if text == "ordinary input")
    }));
    assert!(
        harness
            .llm
            .requests_for(&id)
            .iter()
            .flat_map(user_texts)
            .all(|text| text != "ordinary input"),
        "a refused submission must not enter a model request"
    );

    harness
        .runtime
        .plan_decide(&id, &plan_id, "reject", "")
        .await
        .unwrap();
    assert_eq!(
        harness.llm.requests_for(&id).len(),
        1,
        "rejecting a proposal without held input must not start a run"
    );

    let rejected = harness.runtime.transcript(&id).await.unwrap();
    assert_eq!(rejected.mode.as_deref(), Some("normal"));
    assert!(
        rejected
            .items
            .iter()
            .any(|item| matches!(item, TranscriptItem::Plan { state, .. } if state == "rejected"))
    );

    harness.llm.script(&id, text_reply("unlocked"));
    harness
        .runtime
        .submit_request(&id, "retry-after-reject", "ordinary input".into())
        .await
        .unwrap();
    harness.settle(&id).await;
    assert!(
        harness
            .llm
            .requests_for(&id)
            .iter()
            .flat_map(user_texts)
            .any(|text| text == "ordinary input")
    );
}

#[tokio::test]
async fn completion_keeps_preexisting_follow_up_queued_while_plan_is_pending() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let lease = harness
        .store
        .acquire_lease(&id, "pending-plan-test", 1_000)
        .await
        .unwrap()
        .expect("the idle conversation lease is available");
    let fence = LeaseFence {
        owner: "pending-plan-test".into(),
        epoch: lease,
    };
    let started = harness
        .store
        .admit_command(
            &id,
            CommandAdmission {
                request_id: "start".into(),
                kind: "submit".into(),
                entries: vec![Entry::User {
                    text: "initial work".into(),
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
        .unwrap();
    let start_intent = match started {
        roost_agent::AdmitOutcome::Admitted { seqs, .. } => {
            *seqs.last().expect("activation appends a run intent")
        }
        other => panic!("initial admission failed: {other:?}"),
    };
    harness
        .store
        .admit_command(
            &id,
            CommandAdmission {
                request_id: "seed-pending-plan".into(),
                kind: "submit".into(),
                entries: vec![
                    Entry::QueuedInput {
                        submission_id: "follow-up".into(),
                        text: "keep this draft".into(),
                        attachments: Vec::new(),
                        state: QueuedInputState::Pending,
                    },
                    Entry::User {
                        text: "late steer".into(),
                        attachments: Vec::new(),
                        loop_repeat: false,
                        item_id: Some("late-steer".into()),
                    },
                    Entry::PlanProposal {
                        item_id: "plan".into(),
                        title: "Pending plan".into(),
                        content: "1. decide first".into(),
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

    harness
        .store
        .admit_command(
            &id,
            CommandAdmission {
                request_id: "complete-with-pending-plan".into(),
                kind: "complete".into(),
                entries: Vec::new(),
                intent: IntentChange::Complete,
                plan_admission: PlanAdmission::AllowPending,
                fence: Some(fence),
                expected_intent_seq: Some(start_intent),
                completion_through: Some(start_intent),
            },
        )
        .await
        .unwrap();
    let entries = harness.store.entries(&id).await.unwrap();
    assert!(
        entries.iter().any(|(_, entry)| {
            matches!(entry, Entry::User { text, .. } if text == "late steer")
        })
    );
    assert!(entries.iter().all(|(_, entry)| {
        !matches!(
            entry,
            Entry::QueuedInput {
                state: QueuedInputState::Activated,
                ..
            }
        )
    }));
    assert!(entries.iter().any(|(_, entry)| {
        matches!(
            entry,
            Entry::QueuedInput {
                submission_id,
                state: QueuedInputState::Pending,
                ..
            } if submission_id == "follow-up"
        )
    }));
    assert!(entries.iter().any(|(_, entry)| {
        matches!(
            entry,
            Entry::RunIntent {
                active: false,
                aborted: false,
                ..
            }
        )
    }));
}

#[tokio::test]
async fn proposal_after_a_late_steer_stops_automatic_run_restart() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .tools
        .hold_next
        .store(true, std::sync::atomic::Ordering::SeqCst);
    harness.llm.script(
        &id,
        tool_reply(&[("call", "bash", r#"{"command":"printf waiting"}"#)]),
    );
    harness.llm.script(&id, text_reply("finished current work"));
    harness.submit(&id, "initial work").await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        harness.tools.tool_entered.notified(),
    )
    .await
    .expect("the first tool round starts");
    let credential_gate = harness.llm.hold_credential_check();
    harness.tools.tool_released.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        credential_gate.entered.notified(),
    )
    .await
    .expect("the next request is held before late input is consumed");
    harness
        .runtime
        .submit_request(&id, "late-steer", "do this later".into())
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
    credential_gate.released.notify_one();
    let settled = harness.settle(&id).await;
    assert_eq!(harness.llm.requests_for(&id).len(), 2);
    assert!(settled.items.iter().any(|item| {
        matches!(item, TranscriptItem::User { text, .. } if text == "do this later")
    }));
}
