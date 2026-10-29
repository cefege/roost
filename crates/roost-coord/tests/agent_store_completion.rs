//! Durable completion transitions preserve recovery, Stop, and lease fences.
//! Both memory and SQL admission must preserve accepted work across settlement.
//! The restart module pins the final Stop-versus-restart storage boundary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;

use std::time::{SystemTime, UNIX_EPOCH};

use roost_agent::records::{ConversationRecord, Entry, Mode};
use roost_agent::traits::{
    AdmitOutcome, AgentStore, CommandAdmission, IntentChange, LeaseFence, PlanAdmission,
};
use roost_coord::agent::store::CoordAgentStore;
use roost_protocol::wire::agent_chat::AgentRunState;

#[path = "agent_store_completion/plan_gate.rs"]
mod plan_gate;

#[path = "agent_store_completion/restart.rs"]
mod restart;

fn scratch_root() -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "roost-agent-completion-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create scratch directory");
    path
}

fn conversation(id: &str) -> ConversationRecord {
    ConversationRecord {
        id: id.to_owned(),
        title: format!("title-{id}"),
        worker_fp: "worker-fp".to_owned(),
        worker_label: "desktop".to_owned(),
        worker_os: "linux".to_owned(),
        cwd: "/work/project".to_owned(),
        model: None,
        thinking_level: "medium".to_owned(),
        mode: Mode::Normal,
        pre_plan_model: None,
        parent_id: None,
        agent: None,
        advisor: None,
        read_only: false,
        run_state: AgentRunState::Idle,
        error: None,
        created_ms: 11,
        updated_ms: 22,
        revision: 0,
    }
}
#[tokio::test]
async fn completion_activates_follow_up_behind_a_recoverable_run_boundary() {
    let root = scratch_root();
    let db = db_support::open_test_database(&root)
        .await
        .expect("open database");
    let store = CoordAgentStore::new(db.clone());
    store
        .save_conversation(&conversation("complete"), 0)
        .await
        .expect("insert conversation");
    let epoch = store
        .acquire_lease("complete", "coord-a", 60_000)
        .await
        .unwrap()
        .expect("claim run");
    let fence = LeaseFence {
        owner: "coord-a".into(),
        epoch,
    };
    let AdmitOutcome::Admitted { seqs: started, .. } = store
        .admit_command(
            "complete",
            CommandAdmission {
                request_id: "run-start".into(),
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
        panic!("initial run admission is applied");
    };
    store
        .admit_command(
            "complete",
            CommandAdmission {
                request_id: "follow-up".into(),
                kind: "submit".into(),
                entries: vec![Entry::QueuedInput {
                    submission_id: "follow-up".into(),
                    text: "second".into(),
                    attachments: Vec::new(),
                    state: roost_agent::records::QueuedInputState::Pending,
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

    let completed = store
        .admit_command(
            "complete",
            CommandAdmission {
                request_id: "complete-run".into(),
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
    assert!(matches!(completed, AdmitOutcome::Admitted { .. }));
    let entries = store.entries("complete").await.unwrap();
    let latest_intent = entries
        .iter()
        .rev()
        .find_map(|(sequence, entry)| match entry {
            Entry::RunIntent {
                active,
                through_seq,
                ..
            } => Some((*sequence, *active, *through_seq)),
            _ => None,
        });
    assert!(matches!(latest_intent, Some((_, true, through)) if through == started[0]));
    assert!(entries.iter().any(|(_, entry)| matches!(
        entry,
        Entry::QueuedInput {
            submission_id,
            state: roost_agent::records::QueuedInputState::Activated,
            ..
        } if submission_id == "follow-up"
    )));
    let stale_fence = LeaseFence {
        owner: "coord-a".into(),
        epoch,
    };
    store.release_lease("complete", &stale_fence).await.unwrap();
    store
        .acquire_lease("complete", "coord-b", 60_000)
        .await
        .unwrap();
    assert!(matches!(
        store
            .admit_command(
                "complete",
                CommandAdmission {
                    request_id: "stale-completion".into(),
                    kind: "run-complete".into(),
                    entries: Vec::new(),
                    intent: IntentChange::Complete,
                    plan_admission: PlanAdmission::AllowPending,
                    fence: Some(stale_fence),
                    expected_intent_seq: started.last().copied(),
                    completion_through: started.first().copied(),
                },
            )
            .await,
        Err(roost_agent::AgentError::Fenced(_))
    ));
    db.pool().close().await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn completion_does_not_activate_input_after_a_durable_stop() {
    let root = scratch_root();
    let db = db_support::open_test_database(&root)
        .await
        .expect("open database");
    let store = CoordAgentStore::new(db.clone());
    store
        .save_conversation(&conversation("stopped"), 0)
        .await
        .expect("insert conversation");
    let epoch = store
        .acquire_lease("stopped", "coord-a", 60_000)
        .await
        .unwrap()
        .expect("claim run");
    let fence = LeaseFence {
        owner: "coord-a".into(),
        epoch,
    };
    let AdmitOutcome::Admitted { seqs: started, .. } = store
        .admit_command(
            "stopped",
            CommandAdmission {
                request_id: "run-start".into(),
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
        panic!("initial run admission is applied");
    };
    store
        .admit_command(
            "stopped",
            CommandAdmission {
                request_id: "follow-up".into(),
                kind: "submit".into(),
                entries: vec![Entry::QueuedInput {
                    submission_id: "follow-up".into(),
                    text: "second".into(),
                    attachments: Vec::new(),
                    state: roost_agent::records::QueuedInputState::Pending,
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
    store
        .admit_command(
            "stopped",
            CommandAdmission {
                request_id: "stop".into(),
                kind: "abort".into(),
                entries: Vec::new(),
                intent: IntentChange::Abort,
                plan_admission: PlanAdmission::AllowPending,
                fence: None,
                expected_intent_seq: None,
                completion_through: None,
            },
        )
        .await
        .unwrap();

    let outcome = store
        .admit_command(
            "stopped",
            CommandAdmission {
                request_id: "complete-run".into(),
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
    assert_eq!(outcome, AdmitOutcome::Stopped);
    let entries = store.entries("stopped").await.unwrap();
    assert!(!entries.iter().any(|(_, entry)| matches!(
        entry,
        Entry::QueuedInput {
            state: roost_agent::records::QueuedInputState::Activated,
            ..
        }
    )));
    assert!(matches!(
        entries
            .iter()
            .rev()
            .find(|(_, entry)| matches!(entry, Entry::RunIntent { .. })),
        Some((_, Entry::RunIntent { aborted: true, .. }))
    ));
    db.pool().close().await;
    let _ = std::fs::remove_dir_all(root);
}
