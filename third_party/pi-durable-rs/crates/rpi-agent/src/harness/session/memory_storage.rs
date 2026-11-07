//! Port of `packages/agent/src/harness/session/memory-storage.ts` @ pi 0.82.1 (2efa728) —
//! in-memory `SessionStorage` implementation used by the in-memory session repo.
//!
//! Intentional differences:
//! - The `TMetadata` generic parameter (memory-storage.ts:43) collapses to
//!   [`SessionMetadata`]: upstream only ever instantiates the default
//!   `InMemorySessionStorage<SessionMetadata>`, and the default metadata object
//!   (`{ id: uuidv7_now(), createdAt: new Date().toISOString() }`, memory-storage.ts:61)
//!   cannot be built generically in Rust.
//! - The constructor validates the replayed leaf like upstream (memory-storage.ts:58-60)
//!   and returns `Result<Self, SessionError>` instead of throwing.
//! - `updateLabelCache` / `leafIdAfterEntry` / `generateEntryId` live in `repo_utils`
//!   (shared with the JSONL port; see repo_utils.rs header).
//! - The mutable state sits behind a `tokio::sync::Mutex` because the [`Session`](super::session_facade::Session)
//!   facade calls the storage through `Arc<dyn SessionStorage>` and the write methods
//!   are `&self` (types.rs `SessionStorage` note; session.ts `getStorage`).
//!   `getSessionName` / `getSessionStats` scan `entries` directly under the lock
//!   instead of calling `findEntries` (a tokio mutex is not reentrant).

use std::collections::HashMap;

use async_trait::async_trait;
use rpi_ai::utils::uuid::uuidv7_now;
use tokio::sync::Mutex;

use crate::harness::types::{
    SessionEntryCursorOptions, SessionError, SessionErrorCode, SessionMetadata, SessionStats,
    SessionStorage,
};
use crate::messages::AgentMessage;
use crate::session::{LeafEntry, MessageEntry, SessionEntry};

use super::repo_utils::{generate_entry_id, leaf_id_after_entry, now_iso8601, update_label_cache};

/// Options for [`InMemorySessionStorage::new`] — upstream constructor parameter
/// `{ entries?, metadata? }` (memory-storage.ts:52).
#[derive(Debug, Clone, Default)]
pub struct InMemorySessionStorageOptions {
    pub entries: Option<Vec<SessionEntry>>,
    pub metadata: Option<SessionMetadata>,
}

/// Mutable storage state (interior mutability, see header note).
struct InMemoryState {
    entries: Vec<SessionEntry>,
    by_id: HashMap<String, SessionEntry>,
    labels_by_id: HashMap<String, String>,
    leaf_id: Option<String>,
}

/// `InMemorySessionStorage` (memory-storage.ts:43-188).
pub struct InMemorySessionStorage {
    metadata: SessionMetadata,
    state: Mutex<InMemoryState>,
}

impl InMemorySessionStorage {
    /// Constructor (memory-storage.ts:52-62). Entries are copied, not aliased; the
    /// leaf is replayed from the last entry and must exist in `byId`, otherwise
    /// `invalid_session` (memory-storage.ts:58-60).
    pub fn new(options: InMemorySessionStorageOptions) -> Result<Self, SessionError> {
        let entries = options.entries.unwrap_or_default();
        let mut by_id = HashMap::with_capacity(entries.len());
        let mut labels_by_id = HashMap::new();
        let mut leaf_id: Option<String> = None;
        for entry in &entries {
            by_id.insert(entry.id().to_owned(), entry.clone());
            update_label_cache(&mut labels_by_id, entry);
            leaf_id = leaf_id_after_entry(entry);
        }
        if let Some(id) = &leaf_id
            && !by_id.contains_key(id)
        {
            return Err(SessionError::new(
                SessionErrorCode::InvalidSession,
                format!("Entry {id} not found"),
            ));
        }
        let metadata = options.metadata.unwrap_or(SessionMetadata {
            id: uuidv7_now(),
            created_at: now_iso8601(),
        });
        Ok(Self {
            metadata,
            state: Mutex::new(InMemoryState {
                entries,
                by_id,
                labels_by_id,
                leaf_id,
            }),
        })
    }
}

#[async_trait]
impl SessionStorage for InMemorySessionStorage {
    type Metadata = SessionMetadata;

    /// `getMetadata` (memory-storage.ts:64-66).
    async fn get_metadata(&self) -> Result<Self::Metadata, SessionError> {
        Ok(self.metadata.clone())
    }

    /// `getLeafId` (memory-storage.ts:68-73) — validates the leaf against `byId`.
    async fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        let state = self.state.lock().await;
        if let Some(id) = &state.leaf_id
            && !state.by_id.contains_key(id)
        {
            return Err(SessionError::new(
                SessionErrorCode::InvalidSession,
                format!("Entry {id} not found"),
            ));
        }
        Ok(state.leaf_id.clone())
    }

    /// `setLeafId` (memory-storage.ts:75-89) — records the leaf move by appending a
    /// `leaf` entry (parentId = old leaf, targetId = new leaf).
    async fn set_leaf_id(&self, leaf_id: Option<String>) -> Result<(), SessionError> {
        let mut state = self.state.lock().await;
        if let Some(id) = &leaf_id
            && !state.by_id.contains_key(id)
        {
            return Err(SessionError::new(
                SessionErrorCode::NotFound,
                format!("Entry {id} not found"),
            ));
        }
        let entry = SessionEntry::Leaf(LeafEntry {
            id: generate_entry_id(&state.by_id),
            parent_id: state.leaf_id.clone(),
            timestamp: now_iso8601(),
            target_id: leaf_id.clone(),
        });
        state.entries.push(entry.clone());
        state.by_id.insert(entry.id().to_owned(), entry);
        state.leaf_id = leaf_id;
        Ok(())
    }

    /// `createEntryId` (memory-storage.ts:91-93).
    async fn create_entry_id(&self) -> Result<String, SessionError> {
        let state = self.state.lock().await;
        Ok(generate_entry_id(&state.by_id))
    }

    /// `appendEntry` (memory-storage.ts:95-100).
    async fn append_entry(&self, entry: SessionEntry) -> Result<(), SessionError> {
        let mut state = self.state.lock().await;
        state.entries.push(entry.clone());
        state.by_id.insert(entry.id().to_owned(), entry.clone());
        update_label_cache(&mut state.labels_by_id, &entry);
        state.leaf_id = leaf_id_after_entry(&entry);
        Ok(())
    }

    /// `getEntry` (memory-storage.ts:102-104).
    async fn get_entry(&self, id: &str) -> Result<Option<SessionEntry>, SessionError> {
        let state = self.state.lock().await;
        Ok(state.by_id.get(id).cloned())
    }

    /// `findEntries<TType>` (memory-storage.ts:106-110) — filter by the `entry.type`
    /// tag (types.rs note: per-type extraction has no Rust equivalent).
    async fn find_entries(&self, entry_type: &str) -> Result<Vec<SessionEntry>, SessionError> {
        let state = self.state.lock().await;
        Ok(state
            .entries
            .iter()
            .filter(|entry| entry.type_tag() == entry_type)
            .cloned()
            .collect())
    }

    /// `getLabel` (memory-storage.ts:112-114).
    async fn get_label(&self, id: &str) -> Result<Option<String>, SessionError> {
        let state = self.state.lock().await;
        Ok(state.labels_by_id.get(id).cloned())
    }

    /// `getSessionName` (memory-storage.ts:116-119) — the last `session_info` entry's
    /// trimmed name, or none. Scans `entries` directly (see header note).
    async fn get_session_name(&self) -> Result<Option<String>, SessionError> {
        let state = self.state.lock().await;
        let name = state.entries.iter().rev().find_map(|entry| match entry {
            SessionEntry::SessionInfo(info) => info.name.as_deref().map(str::trim),
            _ => None,
        });
        Ok(match name {
            Some(name) if !name.is_empty() => Some(name.to_owned()),
            _ => None,
        })
    }

    /// `getSessionStats` (memory-storage.ts:121-161) — usage from assistant messages
    /// and `compaction` / `branch_summary` entries. The upstream `typeof ... !==
    /// "number"` guards (memory-storage.ts:140-146) are structurally unreachable for
    /// the typed `Usage` (all fields are numbers); malformed usage in a file fails at
    /// parse time instead (see jsonl_storage.rs header).
    async fn get_session_stats(&self) -> Result<SessionStats, SessionError> {
        let state = self.state.lock().await;
        let mut stats = SessionStats::default();
        for entry in &state.entries {
            if matches!(entry, SessionEntry::Message(_)) {
                stats.message_count = stats.message_count.saturating_add(1);
            }
            let usage = match entry {
                SessionEntry::Message(MessageEntry {
                    message: AgentMessage::Assistant(assistant),
                    ..
                }) => Some(&assistant.usage),
                SessionEntry::Compaction(compaction) => compaction.usage.as_ref(),
                SessionEntry::BranchSummary(branch_summary) => branch_summary.usage.as_ref(),
                _ => None,
            };
            if let Some(usage) = usage {
                stats.cached_tokens = stats.cached_tokens.saturating_add(usage.cache_read);
                stats.uncached_tokens = stats
                    .uncached_tokens
                    .saturating_add(usage.input.saturating_add(usage.cache_write));
                stats.total_tokens = stats.total_tokens.saturating_add(
                    usage
                        .input
                        .saturating_add(usage.output)
                        .saturating_add(usage.cache_read)
                        .saturating_add(usage.cache_write),
                );
                stats.cost_total += usage.cost.total;
            }
        }
        Ok(stats)
    }

    /// `getPathToRootOrCompaction` (memory-storage.ts:163-182) — walk parents from the
    /// leaf, prepending to the path; stops at a compaction with `retainedTail`, at the
    /// compaction's `firstKeptEntryId`, or at the root.
    async fn get_path_to_root_or_compaction(
        &self,
        leaf_id: Option<&str>,
    ) -> Result<Vec<SessionEntry>, SessionError> {
        let state = self.state.lock().await;
        let Some(leaf_id) = leaf_id else {
            return Ok(Vec::new());
        };
        let Some(mut current) = state.by_id.get(leaf_id).cloned() else {
            return Err(SessionError::new(
                SessionErrorCode::NotFound,
                format!("Entry {leaf_id} not found"),
            ));
        };
        let mut stop_at_entry_id: Option<String> = None;
        let mut path: Vec<SessionEntry> = Vec::new();
        loop {
            path.push(current.clone());
            if stop_at_entry_id.as_deref() == Some(current.id()) {
                break;
            }
            if let SessionEntry::Compaction(compaction) = &current {
                // Self-contained compaction (`retainedTail` present): path head.
                if compaction.retained_tail.is_some() {
                    break;
                }
                stop_at_entry_id = compaction.first_kept_entry_id.clone();
            }
            let Some(parent_id) = current.parent_id() else {
                break;
            };
            let Some(parent) = state.by_id.get(parent_id).cloned() else {
                return Err(SessionError::new(
                    SessionErrorCode::InvalidSession,
                    format!("Entry {parent_id} not found"),
                ));
            };
            current = parent;
        }
        path.reverse();
        Ok(path)
    }

    /// `getEntries` (memory-storage.ts:184-188) — JS `Array.prototype.slice` clamps
    /// both bounds, so out-of-range cursors yield `[]`.
    async fn get_entries(
        &self,
        options: SessionEntryCursorOptions,
    ) -> Result<Vec<SessionEntry>, SessionError> {
        let state = self.state.lock().await;
        let len = state.entries.len();
        let start = options.after_entry_seq.unwrap_or(0).min(len);
        let end = match options.limit {
            Some(limit) => start.saturating_add(limit).min(len),
            None => len,
        };
        Ok(state.entries[start..end].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use crate::harness::session::repo_utils::test_support::{
        assistant_message, assistant_message_with_usage, message_entry, usage, user_message,
    };
    use crate::harness::types::SessionEntryCursorOptions;
    use crate::session::{BranchSummaryEntry, CompactionEntry, LeafEntry};

    /// `expect_err` without a `T: Debug` bound (the storage types are not `Debug`).
    fn expect_err<T, E>(result: Result<T, E>, message: &str) -> E {
        match result {
            Ok(_) => panic!("{message}: expected an error"),
            Err(error) => error,
        }
    }
    use super::*;

    fn new_storage() -> InMemorySessionStorage {
        InMemorySessionStorage::new(InMemorySessionStorageOptions::default()).expect("storage")
    }

    fn metadata(id: &str) -> SessionMetadata {
        SessionMetadata {
            id: id.to_owned(),
            created_at: "2026-01-01T00:00:00.000Z".to_owned(),
        }
    }

    async fn entry_ids(storage: &InMemorySessionStorage) -> Vec<String> {
        storage
            .get_entries(SessionEntryCursorOptions::default())
            .await
            .expect("entries")
            .iter()
            .map(|entry| entry.id().to_owned())
            .collect()
    }

    #[tokio::test]
    async fn test_get_metadata_returns_configured_metadata() {
        let storage = InMemorySessionStorage::new(InMemorySessionStorageOptions {
            entries: None,
            metadata: Some(metadata("session-1")),
        })
        .expect("storage");
        assert_eq!(
            storage.get_metadata().await.expect("metadata"),
            metadata("session-1")
        );
    }

    #[tokio::test]
    async fn test_copies_initial_entries_and_persists_leaf_changes() {
        let entry = message_entry(
            "entry-1",
            None,
            "2026-01-01T00:00:00.000Z",
            user_message("one"),
        );
        let mut initial_entries = vec![entry.clone()];
        let storage = InMemorySessionStorage::new(InMemorySessionStorageOptions {
            entries: Some(initial_entries.clone()),
            metadata: None,
        })
        .expect("storage");
        // Mutating the caller's vec must not affect the storage copy
        // (memory-storage.ts:53 — `[...options.entries]`).
        initial_entries.push(message_entry(
            "entry-2",
            None,
            "2026-01-01T00:00:00.000Z",
            user_message("two"),
        ));
        assert_eq!(entry_ids(&storage).await, ["entry-1"]);
        assert_eq!(
            storage.get_leaf_id().await.expect("leaf"),
            Some("entry-1".to_owned())
        );
        storage.set_leaf_id(None).await.expect("set leaf");
        assert_eq!(storage.get_leaf_id().await.expect("leaf"), None);
        let last = storage
            .get_entries(SessionEntryCursorOptions::default())
            .await
            .expect("entries")
            .pop()
            .expect("last entry");
        assert!(matches!(
            last,
            SessionEntry::Leaf(LeafEntry {
                target_id: None,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn test_set_leaf_id_rejects_missing_entries() {
        let storage = new_storage();
        let error = expect_err(
            storage.set_leaf_id(Some("missing".to_owned())).await,
            "error",
        );
        assert_eq!(error.code, SessionErrorCode::NotFound);
        assert!(error.message.contains("Entry missing not found"));
    }

    #[tokio::test]
    async fn test_find_entries_filters_by_type() {
        let entry = message_entry(
            "entry-1",
            None,
            "2026-01-01T00:00:00.000Z",
            user_message("one"),
        );
        let storage = InMemorySessionStorage::new(InMemorySessionStorageOptions {
            entries: Some(vec![entry]),
            metadata: None,
        })
        .expect("storage");
        let found = storage.find_entries("message").await.expect("find");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id(), "entry-1");
        assert!(
            storage
                .find_entries("session_info")
                .await
                .expect("find")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_maintains_label_lookup() {
        let entry = message_entry(
            "entry-1",
            None,
            "2026-01-01T00:00:00.000Z",
            user_message("one"),
        );
        let storage = InMemorySessionStorage::new(InMemorySessionStorageOptions {
            entries: Some(vec![entry]),
            metadata: None,
        })
        .expect("storage");
        assert_eq!(storage.get_label("entry-1").await.expect("label"), None);
        storage
            .append_entry(SessionEntry::Label(crate::session::LabelEntry {
                id: "label-1".to_owned(),
                parent_id: Some("entry-1".to_owned()),
                timestamp: "2026-01-01T00:00:01.000Z".to_owned(),
                target_id: "entry-1".to_owned(),
                label: Some("checkpoint".to_owned()),
            }))
            .await
            .expect("append");
        assert_eq!(
            storage.get_label("entry-1").await.expect("label"),
            Some("checkpoint".to_owned())
        );
        storage
            .append_entry(SessionEntry::Label(crate::session::LabelEntry {
                id: "label-2".to_owned(),
                parent_id: Some("label-1".to_owned()),
                timestamp: "2026-01-01T00:00:02.000Z".to_owned(),
                target_id: "entry-1".to_owned(),
                label: None,
            }))
            .await
            .expect("append");
        assert_eq!(storage.get_label("entry-1").await.expect("label"), None);
    }

    #[tokio::test]
    async fn test_get_session_stats_includes_summary_entry_usage() {
        let assistant = message_entry(
            "assistant",
            None,
            "2026-01-01T00:00:00.000Z",
            assistant_message_with_usage("reply", usage(10, 20, 30, 40, 100, 1.0)),
        );
        let compaction = SessionEntry::Compaction(CompactionEntry {
            id: "compaction".to_owned(),
            parent_id: Some("assistant".to_owned()),
            timestamp: "2026-01-01T00:00:01.000Z".to_owned(),
            summary: "summary".to_owned(),
            first_kept_entry_id: Some("assistant".to_owned()),
            tokens_before: 1234,
            retained_tail: None,
            details: None,
            usage: Some(usage(1, 2, 3, 4, 10, 0.1)),
            from_hook: None,
            system_message: None,
        });
        let branch_summary = SessionEntry::BranchSummary(BranchSummaryEntry {
            id: "branch-summary".to_owned(),
            parent_id: Some("compaction".to_owned()),
            timestamp: "2026-01-01T00:00:02.000Z".to_owned(),
            from_id: "assistant".to_owned(),
            summary: "branch".to_owned(),
            details: None,
            usage: Some(usage(5, 6, 7, 8, 26, 0.26)),
            from_hook: None,
        });
        let storage = InMemorySessionStorage::new(InMemorySessionStorageOptions {
            entries: Some(vec![assistant, compaction, branch_summary]),
            metadata: None,
        })
        .expect("storage");
        assert_eq!(
            storage.get_session_stats().await.expect("stats"),
            SessionStats {
                message_count: 1,
                cached_tokens: 40,
                uncached_tokens: 68,
                total_tokens: 136,
                cost_total: 1.36,
            }
        );
    }

    #[tokio::test]
    async fn test_get_path_to_root_or_compaction_walks_to_root_or_retained_tail() {
        let root = message_entry(
            "root",
            None,
            "2026-01-01T00:00:00.000Z",
            user_message("root"),
        );
        let child = message_entry(
            "child",
            Some("root"),
            "2026-01-01T00:00:00.000Z",
            assistant_message("child"),
        );
        let compaction = SessionEntry::Compaction(CompactionEntry {
            id: "compaction".to_owned(),
            parent_id: Some("child".to_owned()),
            timestamp: "2026-01-01T00:00:01.000Z".to_owned(),
            summary: "summary".to_owned(),
            first_kept_entry_id: Some("child".to_owned()),
            tokens_before: 1234,
            retained_tail: Some(vec![assistant_message("child")]),
            details: None,
            usage: None,
            from_hook: None,
            system_message: None,
        });
        let after = message_entry(
            "after-compaction",
            Some("compaction"),
            "2026-01-01T00:00:00.000Z",
            user_message("after"),
        );
        let storage = InMemorySessionStorage::new(InMemorySessionStorageOptions {
            entries: Some(vec![root, child, compaction, after]),
            metadata: None,
        })
        .expect("storage");
        let path_ids = |entries: Vec<SessionEntry>| {
            entries
                .iter()
                .map(|entry| entry.id().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            path_ids(
                storage
                    .get_path_to_root_or_compaction(Some("child"))
                    .await
                    .expect("path")
            ),
            ["root", "child"]
        );
        assert_eq!(
            path_ids(
                storage
                    .get_path_to_root_or_compaction(Some("after-compaction"))
                    .await
                    .expect("path")
            ),
            ["compaction", "after-compaction"]
        );
        assert!(
            storage
                .get_path_to_root_or_compaction(None)
                .await
                .expect("path")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_get_path_to_root_or_compaction_rejects_unknown_leaf_and_parent() {
        let storage = new_storage();
        let error = expect_err(
            storage
                .get_path_to_root_or_compaction(Some("missing"))
                .await,
            "error",
        );
        assert_eq!(error.code, SessionErrorCode::NotFound);
        storage
            .append_entry(message_entry(
                "orphan",
                Some("ghost"),
                "t",
                user_message("x"),
            ))
            .await
            .expect("append");
        let error = expect_err(
            storage.get_path_to_root_or_compaction(Some("orphan")).await,
            "error",
        );
        assert_eq!(error.code, SessionErrorCode::InvalidSession);
        assert!(error.message.contains("Entry ghost not found"));
    }

    #[tokio::test]
    async fn test_constructor_rejects_orphan_replayed_leaf() {
        // Constructor validation (memory-storage.ts:58-60).
        let entries = vec![
            message_entry("m1", None, "t", user_message("x")),
            SessionEntry::Leaf(LeafEntry {
                id: "l1".to_owned(),
                parent_id: Some("m1".to_owned()),
                timestamp: "t".to_owned(),
                target_id: Some("ghost".to_owned()),
            }),
        ];
        let error = expect_err(
            InMemorySessionStorage::new(InMemorySessionStorageOptions {
                entries: Some(entries),
                metadata: None,
            }),
            "error",
        );
        assert_eq!(error.code, SessionErrorCode::InvalidSession);
        assert!(error.message.contains("Entry ghost not found"));
    }
}
