//! Detached in-memory, JSONL and SQLite backends, plus the shared scan helpers.

pub mod jsonl;
pub mod memory;
pub mod scan;
pub mod sqlite;

pub use crate::fs::{DirEntry, DirEntryKind, FileSystem, StdFileSystem};
pub use jsonl::{JsonlStorage, JsonlStorageOptions};
pub use memory::{MemoryStorage, PreparedMemoryCommit};
pub use scan::{ScanStart, scan_start};
pub use sqlite::{
    CURRENT_SQLITE_SCHEMA_VERSION, DEFAULT_BUSY_TIMEOUT_MS, DEFAULT_WAL_AUTO_CHECKPOINT_PAGES,
    SqliteDatabase, SqliteDatabaseOptions, SqliteExecutor, SqliteMigration, SqliteStorage,
    SqliteTransaction, SqliteValue, SqliteValueRef, apply_sqlite_migrations,
    open_node_sqlite_database, open_node_sqlite_storage, sqlite_migrations,
};

use crate::types::{Cursor, DocumentPoint, DocumentRecord, DocumentScope, Page, ScanOrder};

/// JavaScript `Number.MAX_SAFE_INTEGER`: the ID namespace minting limit.
pub(crate) const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Build a page from `limit + 1`-sized candidate values (upstream `page`).
pub(crate) fn page_values<T>(
    mut values: Vec<T>,
    limit: usize,
    order: ScanOrder,
    id_of: impl Fn(&T) -> u64,
) -> Page<T> {
    if limit == 0 {
        // Upstream `page()` throws on a zero limit (it indexes the empty
        // slice); rpi returns an empty page instead (form difference).
        return Page {
            items: Vec::new(),
            next: None,
        };
    }
    if values.len() <= limit {
        return Page {
            items: values,
            next: None,
        };
    }
    let last = id_of(&values[limit - 1]);
    values.truncate(limit);
    Page {
        items: values,
        next: Some(Cursor::after_id(last, order)),
    }
}

/// Validate a caller cursor against the safe-integer ID space (upstream
/// `cursorId`); document scans apply it directly instead of `scan_start`.
pub(crate) fn validate_cursor(cursor: Option<&Cursor>) -> Result<(), crate::errors::StorageError> {
    if let Some(cursor) = cursor
        && cursor.after > MAX_SAFE_INTEGER
    {
        return Err(crate::errors::StorageError::InvalidCursor);
    }
    Ok(())
}

pub(crate) fn insert_sorted<T: Ord + Copy>(ids: &mut Vec<T>, id: T) {
    if ids.last().is_none_or(|last| *last < id) {
        ids.push(id);
        return;
    }
    let index = ids.partition_point(|existing| *existing < id);
    ids.insert(index, id);
}

pub(crate) fn remove_sorted<T: Ord + Copy>(ids: &mut Vec<T>, id: T) {
    let index = ids.partition_point(|existing| *existing < id);
    if ids.get(index) == Some(&id) {
        ids.remove(index);
    }
}

pub(crate) fn insert_map_id<K: Eq + std::hash::Hash + Copy, T: Ord + Copy>(
    index: &mut std::collections::HashMap<K, Vec<T>>,
    key: K,
    id: T,
) {
    insert_sorted(index.entry(key).or_default(), id);
}

pub(crate) fn scope_key(scope: &DocumentScope) -> String {
    match scope {
        DocumentScope::Session => {
            serde_json::to_string(&serde_json::json!(["session"])).unwrap_or_default()
        }
        DocumentScope::Conversation {
            conversation_id, ..
        } => serde_json::to_string(&serde_json::json!(["conversation", conversation_id.get()]))
            .unwrap_or_default(),
        DocumentScope::Task { task_id } => {
            serde_json::to_string(&serde_json::json!(["task", task_id.get()])).unwrap_or_default()
        }
    }
}

pub(crate) fn address_key(address: &crate::types::DocumentAddress) -> String {
    let key_part = match &address.key {
        None => serde_json::json!(["singleton"]),
        Some(key) => serde_json::json!(["family", key]),
    };
    serde_json::to_string(&serde_json::json!([
        address.kind,
        scope_key(&address.scope),
        key_part
    ]))
    .unwrap_or_default()
}

pub(crate) fn record_address_key(record: &crate::types::DocumentCreate) -> String {
    address_key(&crate::types::DocumentAddress {
        kind: record.kind.clone(),
        scope: record.scope.clone(),
        key: record.key.clone(),
    })
}

pub(crate) fn record_address_key_of(record: &DocumentRecord) -> String {
    address_key(&crate::types::DocumentAddress {
        kind: record.kind.clone(),
        scope: record.scope.clone(),
        key: record.key.clone(),
    })
}

pub(crate) fn is_alive_at(record: &DocumentRecord, at: DocumentPoint) -> bool {
    match at {
        DocumentPoint::Current(_) => record.retired_at.is_none(),
        DocumentPoint::At(seq) => {
            record.created_at <= seq && record.retired_at.is_none_or(|retired| seq < retired)
        }
    }
}

pub(crate) fn is_current_only(record: &DocumentRecord) -> bool {
    !matches!(record.scope, DocumentScope::Conversation { .. })
        || record.history == Some(crate::types::DocumentHistory::Latest)
}

pub(crate) fn is_current_only_create(record: &crate::types::DocumentCreate) -> bool {
    !matches!(record.scope, DocumentScope::Conversation { .. })
        || record.history == Some(crate::types::DocumentHistory::Latest)
}
