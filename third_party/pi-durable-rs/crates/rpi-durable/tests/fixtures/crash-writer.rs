//! Crash-survival helper: opens durable SQLite storage, commits one entry,
//! writes a ready marker and then sleeps until killed (V17-13 FR-E R4
//! `kill -9` anchor).

use rpi_durable::chord::context::BACKGROUND_CONTEXT;
use rpi_durable::storage::{SqliteDatabaseOptions, open_node_sqlite_storage};
use rpi_durable::types::{ConversationRecord, EntryRecord, Storage, StorageWrite};
use rpi_durable::{EntryId, ROOT_CONVERSATION_ID, id_from_number};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("database path");
    let marker = args.next().expect("ready marker path");
    let storage = open_node_sqlite_storage(&path, SqliteDatabaseOptions::default())
        .await
        .expect("open storage");
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
    let entry_id: EntryId = id_from_number(storage.mint_id().await.expect("mint"));
    storage
        .commit(
            &[StorageWrite::Entry {
                value: EntryRecord::new(entry_id, ROOT_CONVERSATION_ID, "crash-entry"),
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("entry commit");
    std::fs::write(marker, entry_id.get().to_string()).expect("write marker");
    // Keep the process alive until the parent kills it; do not close the
    // storage so the crash happens with the connection open.
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}
