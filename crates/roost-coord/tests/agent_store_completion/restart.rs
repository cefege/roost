//! Storage ordering when Stop races an automatically activated next run.
//! Exercises the same precondition against memory and coordinator stores.
//! Refusing stale work must not reserve its identity or erase an explicit Stop.

use roost_agent::InMemoryAgentStore;
use roost_agent::records::Entry;
use roost_agent::traits::{AdmitOutcome, AgentStore, CommandAdmission, IntentChange, LeaseFence};
use roost_coord::agent::store::CoordAgentStore;

#[tokio::test]
async fn stop_supersedes_an_automatic_restart_in_both_stores() {
    assert_stop_blocks_stale_restart(&InMemoryAgentStore::default(), "memory").await;
    let root = super::scratch_root();
    let db = super::db_support::open_test_database(&root).await.unwrap();
    assert_stop_blocks_stale_restart(&CoordAgentStore::new(db.clone()), "sql").await;
    db.pool().close().await;
    let _ = std::fs::remove_dir_all(root);
}

async fn assert_stop_blocks_stale_restart(store: &dyn AgentStore, id: &str) {
    store
        .save_conversation(&super::conversation(id), 0)
        .await
        .unwrap();
    let epoch = store
        .acquire_lease(id, "owner", 60_000)
        .await
        .unwrap()
        .unwrap();
    let fence = LeaseFence {
        owner: "owner".to_owned(),
        epoch,
    };
    store
        .append_entry(
            id,
            &Entry::User {
                text: "Accepted next input".to_owned(),
                loop_repeat: false,
                item_id: Some("next".to_owned()),
            },
            &[],
            Some(&fence),
        )
        .await
        .unwrap();
    let completed_boundary = store
        .append_entry(
            id,
            &Entry::RunIntent {
                active: true,
                through_seq: 0,
                aborted: false,
                yielded: false,
            },
            &[],
            Some(&fence),
        )
        .await
        .unwrap();
    store
        .admit_command(
            id,
            CommandAdmission {
                request_id: "stop".to_owned(),
                kind: "abort".to_owned(),
                entries: Vec::new(),
                intent: IntentChange::Abort,
                fence: None,
                expected_intent_seq: None,
                completion_through: None,
            },
        )
        .await
        .unwrap();
    let stopped = store.entries(id).await.unwrap();
    let restart = |boundary| CommandAdmission {
        request_id: "automatic-restart".to_owned(),
        kind: "run-restart".to_owned(),
        entries: Vec::new(),
        intent: IntentChange::RestartIfCurrent,
        fence: Some(fence.clone()),
        expected_intent_seq: Some(boundary),
        completion_through: None,
    };
    assert_eq!(
        store
            .admit_command(id, restart(completed_boundary))
            .await
            .unwrap(),
        AdmitOutcome::Stopped
    );
    assert_eq!(
        store.entries(id).await.unwrap(),
        stopped,
        "a refused restart must not append an active intent"
    );
    let AdmitOutcome::Admitted { seqs, .. } = store
        .admit_command(
            id,
            CommandAdmission {
                request_id: "explicit-new-input".to_owned(),
                kind: "submit".to_owned(),
                entries: vec![Entry::User {
                    text: "Continue now".to_owned(),
                    loop_repeat: false,
                    item_id: Some("explicit-new-input".to_owned()),
                }],
                intent: IntentChange::Activate,
                fence: None,
                expected_intent_seq: None,
                completion_through: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("an explicit new input must still be admitted after Stop");
    };
    assert!(
        matches!(
            store
                .admit_command(id, restart(*seqs.last().unwrap()))
                .await
                .unwrap(),
            AdmitOutcome::Admitted { .. }
        ),
        "a refused stale restart must not reserve its request identity"
    );
}
