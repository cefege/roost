//! Memory backend tests: shared conformance suite + upstream memory-storage.rs
//! specific anchors.

mod test_support;

use std::sync::Arc;

use rpi_durable::chord::context::BACKGROUND_CONTEXT;
use rpi_durable::ids::{EntryId, ROOT_CONVERSATION_ID, id_from_number, seq_from_number};
use rpi_durable::storage::MemoryStorage;
use rpi_durable::testing::run_storage_conformance;
use rpi_durable::types::{ConversationRecord, EntryRecord, Storage, StorageWrite};

#[tokio::test]
async fn memory_storage_conformance() {
    let result = run_storage_conformance(|| async {
        Ok(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>)
    })
    .await;
    assert!(
        result.is_ok(),
        "conformance failures:\n{}",
        result.err().unwrap_or_default()
    );
}

#[tokio::test]
async fn memory_prepared_commit_is_detached_and_idempotent() {
    let storage = MemoryStorage::new();
    storage
        .commit(
            &[StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("root commit");
    let entry_id: EntryId = id_from_number(2);
    let mut prepared = storage
        .prepare_commit(
            vec![StorageWrite::Entry {
                value: {
                    let mut entry = EntryRecord::new(entry_id, ROOT_CONVERSATION_ID, "test");
                    entry.data = Some(serde_json::json!({ "nested": [1] }));
                    entry
                },
            }],
            None,
        )
        .expect("prepare");
    assert_eq!(prepared.seq(), seq_from_number(2));
    let first = storage.apply_prepared(&mut prepared).expect("apply");
    let second = storage.apply_prepared(&mut prepared).expect("re-apply");
    assert_eq!(first, second);
    let stored = storage
        .entry(entry_id, &BACKGROUND_CONTEXT)
        .await
        .expect("read")
        .expect("entry");
    assert_eq!(
        stored.entry.data,
        Some(serde_json::json!({ "nested": [1] }))
    );
}

#[tokio::test]
async fn memory_rejects_commit_after_close() {
    let storage = MemoryStorage::new();
    storage.close(&BACKGROUND_CONTEXT).await.expect("close");
    assert!(storage.commit(&[], &BACKGROUND_CONTEXT).await.is_err());
}

#[tokio::test]
async fn memory_document_scan_rejects_cursors_beyond_the_safe_integer() {
    let storage = MemoryStorage::new();
    let cursor = rpi_durable::types::Cursor {
        after: 9_007_199_254_740_992,
        order: None,
    };
    let result = storage
        .scan_documents(
            rpi_durable::types::DocumentQuery {
                scope: rpi_durable::types::DocumentScope::Session,
                at: rpi_durable::types::DocumentPoint::CURRENT,
                kind: None,
            },
            10,
            Some(cursor),
            &BACKGROUND_CONTEXT,
        )
        .await;
    assert!(
        result
            .expect_err("invalid cursor")
            .to_string()
            .contains("Invalid storage cursor"),
        "document scans must reject unsafe cursor IDs"
    );
}
