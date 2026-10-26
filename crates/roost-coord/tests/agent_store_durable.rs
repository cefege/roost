//! Durable SQL batches, IDs, and transcript projection on SQLite or Postgres.
//!
//! The shared database fixture also exercises persistence across pool reopen.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;

use std::time::{SystemTime, UNIX_EPOCH};

use roost_agent::durable::StorageWrite;
use roost_agent::records::{ConversationRecord, Entry, Mode};
use roost_agent::traits::{AgentStore, LeaseFence};
use roost_coord::agent::store::CoordAgentStore;
use roost_protocol::wire::agent_chat::AgentRunState;
use serde_json::json;

fn scratch_root() -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "roost-agent-store-durable-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create scratch directory");
    root
}

fn conversation() -> ConversationRecord {
    ConversationRecord {
        id: "durable".to_owned(),
        title: "durable".to_owned(),
        worker_fp: "worker".to_owned(),
        worker_label: "desktop".to_owned(),
        worker_os: "linux".to_owned(),
        cwd: "/work".to_owned(),
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
        created_ms: 1,
        updated_ms: 1,
        revision: 0,
    }
}

#[tokio::test]
async fn durable_ids_batches_and_journal_projection_survive_reopen_and_reject_replay() {
    let root = scratch_root();
    let db = db_support::open_test_database(&root)
        .await
        .expect("open database");
    let store = CoordAgentStore::new(db.clone());
    store
        .save_conversation(&conversation(), 0)
        .await
        .expect("save conversation");
    let mut other = conversation();
    other.id = "durable-other".to_owned();
    store
        .save_conversation(&other, 0)
        .await
        .expect("save second conversation");
    let epoch = store
        .acquire_lease("durable", "coordinator", 60_000)
        .await
        .expect("acquire lease")
        .expect("lease acquired");
    let other_epoch = store
        .acquire_lease("durable-other", "coordinator", 60_000)
        .await
        .expect("acquire other lease")
        .expect("other lease acquired");
    let fence = LeaseFence {
        owner: "coordinator".to_owned(),
        epoch,
    };
    let other_fence = LeaseFence {
        owner: "coordinator".to_owned(),
        epoch: other_epoch,
    };

    let (first_result, second_result, other_result) = tokio::join!(
        store.mint_durable_id("durable", &fence),
        store.mint_durable_id("durable", &fence),
        store.mint_durable_id("durable-other", &other_fence)
    );
    let first_result = first_result.expect("mint first concurrent ID");
    let second_result = second_result.expect("mint second concurrent ID");
    let other_id = other_result.expect("mint ID in other conversation namespace");
    let (first_id, second_id) = if first_result < second_result {
        (first_result, second_result)
    } else {
        (second_result, first_result)
    };
    assert_ne!(first_id, second_id);
    assert_eq!(
        first_id, other_id,
        "IDs are unique within each conversation namespace"
    );

    let canonical_write: StorageWrite = serde_json::from_value(json!({
        "type": "entry",
        "value": {
            "id": 41,
            "conversationId": 1,
            "kind": "pi.assistant",
            "model": [],
            "data": { "text": "kernel state" }
        }
    }))
    .expect("decode canonical kernel entry write");
    let writes = [canonical_write.clone()];
    let entry = Entry::Notice {
        level: "info".to_owned(),
        title: "durable projection".to_owned(),
        body: "committed with its batch".to_owned(),
    };
    let commit = store
        .append_durable_batch("durable", 0, &writes, std::slice::from_ref(&entry), &fence)
        .await
        .expect("commit durable batch");
    assert_eq!(commit.sequence, 1);
    assert_eq!(commit.entry_sequences, vec![1]);
    let expected_projection = vec![(1, entry.clone())];
    assert_eq!(
        store.entries("durable").await.expect("read journal"),
        expected_projection
    );
    let expected_batch = roost_agent::durable::DurableBatch {
        sequence: 1,
        writes: writes.to_vec(),
        entry_sequences: vec![1],
    };
    let expected_batches =
        serde_json::to_value([expected_batch]).expect("serialize expected batches");
    assert_eq!(
        serde_json::to_value(
            store
                .durable_batches("durable")
                .await
                .expect("read batches"),
        )
        .expect("serialize batches"),
        expected_batches
    );

    let rejected_entry = Entry::Notice {
        level: "warning".to_owned(),
        title: "must not project".to_owned(),
        body: "stale sequence".to_owned(),
    };
    assert!(matches!(
        store
            .append_durable_batch(
                "durable",
                0,
                &writes,
                std::slice::from_ref(&rejected_entry),
                &fence,
            )
            .await,
        Err(roost_agent::AgentError::Conflict(_))
    ));
    assert_eq!(
        store.entries("durable").await.expect("journal unchanged"),
        expected_projection
    );
    assert_eq!(
        serde_json::to_value(
            store
                .durable_batches("durable")
                .await
                .expect("batch unchanged"),
        )
        .expect("serialize batches"),
        expected_batches
    );

    sqlx::query("UPDATE agent_run_leases SET expires_ms=0 WHERE conversation_id=$1")
        .bind("durable")
        .execute(db.pool())
        .await
        .expect("expire lease");
    assert!(matches!(
        store.mint_durable_id("durable", &fence).await,
        Err(roost_agent::AgentError::Fenced(_))
    ));
    assert!(matches!(
        store
            .append_durable_batch(
                "durable",
                1,
                &writes,
                std::slice::from_ref(&rejected_entry),
                &fence,
            )
            .await,
        Err(roost_agent::AgentError::Fenced(_))
    ));
    assert!(matches!(
        store
            .append_entry("durable", &rejected_entry, &[], Some(&fence))
            .await,
        Err(roost_agent::AgentError::Fenced(_))
    ));
    assert_eq!(
        store
            .entries("durable")
            .await
            .expect("stale writes leave journal unchanged"),
        expected_projection
    );
    assert_eq!(
        serde_json::to_value(
            store
                .durable_batches("durable")
                .await
                .expect("stale writes leave batches unchanged"),
        )
        .expect("serialize batches"),
        expected_batches
    );
    store
        .release_lease("durable", &fence)
        .await
        .expect("release before reopen");

    db.pool().close().await;
    drop(store);
    drop(db);
    let reopened = db_support::open_test_database(&root)
        .await
        .expect("reopen database");
    let reopened_store = CoordAgentStore::new(reopened.clone());
    let reopened_epoch = reopened_store
        .acquire_lease("durable", "coordinator", 60_000)
        .await
        .expect("acquire reopened lease")
        .expect("released database lease expired");
    let reopened_fence = LeaseFence {
        owner: "coordinator".to_owned(),
        epoch: reopened_epoch,
    };
    assert!(matches!(
        reopened_store.mint_durable_id("durable", &fence).await,
        Err(roost_agent::AgentError::Fenced(_))
    ));
    let third_id = reopened_store
        .mint_durable_id("durable", &reopened_fence)
        .await
        .expect("mint after reopen");
    assert!(third_id > second_id);
    assert_eq!(
        serde_json::to_value(
            reopened_store
                .durable_batches("durable")
                .await
                .expect("read persisted batch"),
        )
        .expect("serialize batches"),
        expected_batches,
        "the canonical kernel write survives pool reopen"
    );
    assert_eq!(
        reopened_store
            .entries("durable")
            .await
            .expect("read persisted journal"),
        expected_projection,
        "the native transcript projection survives pool reopen"
    );
    sqlx::query("UPDATE agent_durable_ids SET next_id=$1 WHERE conversation_id=$2")
        .bind(9_007_199_254_740_990_i64)
        .bind("durable")
        .execute(reopened.pool())
        .await
        .expect("reach the last safe ID");
    assert_eq!(
        reopened_store
            .mint_durable_id("durable", &reopened_fence)
            .await
            .expect("mint the final safe ID"),
        9_007_199_254_740_991
    );
    assert!(matches!(
        reopened_store
            .mint_durable_id("durable", &reopened_fence)
            .await,
        Err(roost_agent::AgentError::Store(_))
    ));
    reopened.pool().close().await;
    let _ = std::fs::remove_dir_all(root);
}
