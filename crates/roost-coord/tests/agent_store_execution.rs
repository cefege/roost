//! The replica-safe half of the coordinator agent store: compare-and-set
//! conversation rows, run-lease fencing, and once-only command admission, on
//! whichever backend `ROOST_TEST_DATABASE_URL` selects.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;

use std::time::{SystemTime, UNIX_EPOCH};

use roost_agent::records::{ConversationRecord, Entry, Mode};
use roost_agent::traits::{
    AdmitOutcome, AgentStore, CommandAdmission, IntentChange, LeaseFence, PlanAdmission,
};
use roost_coord::agent::store::CoordAgentStore;
use roost_protocol::wire::agent_chat::AgentRunState;

fn scratch_root() -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "roost-agent-store-execution-{}-{stamp}",
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
async fn stale_revisions_conflict_superseded_leases_are_fenced_and_commands_admit_once() {
    let root = scratch_root();
    let db = db_support::open_test_database(&root)
        .await
        .expect("open database");
    let store = CoordAgentStore::new(db.clone());
    let mut record = conversation("shared");
    let first = store.save_conversation(&record, 0).await.expect("insert");
    assert!(matches!(
        store.save_conversation(&record, 0).await,
        Err(roost_agent::AgentError::Conflict(_))
    ));
    record.title = "renamed".into();
    let second = store
        .save_conversation(&record, first)
        .await
        .expect("update");
    assert!(
        matches!(
            store.save_conversation(&record, first).await,
            Err(roost_agent::AgentError::Conflict(_))
        ),
        "a write from the superseded revision {first} must not overwrite revision {second}"
    );

    let old = store
        .acquire_lease("shared", "coord-a", 60_000)
        .await
        .unwrap()
        .expect("a free lease is taken");
    assert_eq!(
        store
            .acquire_lease("shared", "coord-b", 60_000)
            .await
            .unwrap(),
        None,
        "a live foreign lease is refused"
    );
    let old_fence = LeaseFence {
        owner: "coord-a".into(),
        epoch: old,
    };
    store.release_lease("shared", &old_fence).await.unwrap();
    let new = store
        .acquire_lease("shared", "coord-b", 60_000)
        .await
        .unwrap()
        .expect("a released lease is taken");
    assert!(new > old);
    let note = Entry::Notice {
        level: "info".into(),
        title: "stale".into(),
        body: String::new(),
    };
    assert!(matches!(
        store
            .append_entry("shared", &note, &[], Some(&old_fence))
            .await,
        Err(roost_agent::AgentError::Fenced(_))
    ));
    assert!(
        !store
            .renew_lease("shared", &old_fence, 60_000)
            .await
            .unwrap()
    );

    let admission = CommandAdmission {
        request_id: "submit-1".into(),
        kind: "submit".into(),
        entries: vec![Entry::User {
            text: "once".into(),
            attachments: Vec::new(),
            loop_repeat: false,
            item_id: None,
        }],
        intent: IntentChange::Activate,
        plan_admission: PlanAdmission::AllowPending,
        fence: None,
        expected_intent_seq: None,
        completion_through: None,
    };
    let AdmitOutcome::Admitted { seqs, .. } = store
        .admit_command("shared", admission.clone())
        .await
        .unwrap()
    else {
        panic!("first admission is applied");
    };
    assert_eq!(seqs, vec![1, 2]);
    assert_eq!(
        store.admit_command("shared", admission).await.unwrap(),
        AdmitOutcome::Duplicate { result_json: None }
    );
    assert_eq!(
        store.entries("shared").await.unwrap(),
        vec![
            (
                1,
                Entry::User {
                    text: "once".into(),
                    attachments: Vec::new(),
                    loop_repeat: false,
                    item_id: None,
                }
            ),
            (
                2,
                Entry::RunIntent {
                    active: true,
                    through_seq: 1,
                    aborted: false,
                    yielded: false
                }
            ),
        ]
    );
    db.pool().close().await;
    let _ = std::fs::remove_dir_all(root);
}
