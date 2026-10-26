//! Portable SQLite implementation of the storage contract
//! (port of `src/storage/sqlite/storage.ts` @ `abe508e1b`).

use async_trait::async_trait;
use std::sync::Mutex;

use crate::chord::context::Context;
use crate::chord::delta::{Op, apply_immutable_batches};
use crate::chord::json::{JsonObject, JsonValue};
use crate::errors::{StorageError, StorageResult};
use crate::ids::{ConversationId, DocumentId, EntryId, Seq, SubmissionId, TaskId, id_from_number};
use crate::storage::scan::{ScanStart, scan_start};
use crate::storage::sqlite::database::{SqliteDatabase, SqliteTransaction, SqliteValue};
use crate::storage::sqlite::migrations::{apply_sqlite_migrations, sqlite_migrations};
use crate::storage::{is_alive_at, is_current_only, page_values};
use crate::types::{
    ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentContent,
    DocumentCreate, DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope, EntryQuery,
    EntryRecord, Page, ScanOrder, Storage, StorageWrite, StoredDocument, StoredEntry,
    SubmissionQuery, SubmissionRecord, TaskQuery, TaskRecord,
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn encode_json<T: serde::Serialize>(value: &T) -> StorageResult<String> {
    Ok(serde_json::to_string(value)?)
}

/// Some SQLite bindings replace lone UTF-16 surrogates. JSON encoding keeps
/// indexed identities lossless.
fn encode_indexed_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}

struct ScopeColumns {
    scope_kind: &'static str,
    owner_id: i64,
}

fn scope_columns(scope: &DocumentScope) -> ScopeColumns {
    match scope {
        DocumentScope::Session => ScopeColumns {
            scope_kind: "session",
            owner_id: 0,
        },
        DocumentScope::Conversation {
            conversation_id, ..
        } => ScopeColumns {
            scope_kind: "conversation",
            owner_id: conversation_id.get() as i64,
        },
        DocumentScope::Task { task_id } => ScopeColumns {
            scope_kind: "task",
            owner_id: task_id.get() as i64,
        },
    }
}

struct AddressParts {
    kind: String,
    scope_kind: &'static str,
    owner_id: i64,
    family: i64,
    key_value: String,
}

fn address_parts(kind: &str, scope: &DocumentScope, key: Option<&str>) -> AddressParts {
    let scope = scope_columns(scope);
    AddressParts {
        kind: encode_indexed_string(kind),
        scope_kind: scope.scope_kind,
        owner_id: scope.owner_id,
        family: if key.is_some() { 1 } else { 0 },
        key_value: encode_indexed_string(key.unwrap_or("")),
    }
}

fn address_parts_of(address: &DocumentAddress) -> AddressParts {
    address_parts(&address.kind, &address.scope, address.key.as_deref())
}

fn scan_sql(start: ScanStart) -> (&'static str, i64, &'static str) {
    match start.order {
        ScanOrder::Ascending => (
            "id > ?",
            start.after.map(|after| after as i64).unwrap_or(-1),
            "ASC",
        ),
        ScanOrder::Descending => (
            "id < ?",
            start.after.map(|after| after as i64).unwrap_or(i64::MAX),
            "DESC",
        ),
    }
}

#[derive(Debug, Clone, Default)]
struct DocumentAction {
    create: Option<DocumentCreate>,
    content: Option<DocumentContent>,
    copy: Option<crate::types::DocumentCopySource>,
    retire: bool,
}

#[derive(Default)]
struct Admission {
    closed: bool,
    admitted_reads: usize,
}

/// Portable SQLite implementation of the storage contract.
pub struct SqliteStorage {
    db: SqliteDatabase,
    next_id: Mutex<u64>,
    admission: Mutex<Admission>,
    drained: tokio::sync::Notify,
    close_lock: tokio::sync::Mutex<()>,
}

/// Keeps one admitted multi-query read alive until it settles so `close()` can
/// wait for it (upstream `admitRead`).
struct ReadGuard<'a> {
    storage: &'a SqliteStorage,
}

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut admission) = self.storage.admission.lock() {
            admission.admitted_reads = admission.admitted_reads.saturating_sub(1);
            if admission.admitted_reads == 0 {
                drop(admission);
                self.storage.drained.notify_waiters();
            }
        }
    }
}

impl SqliteStorage {
    /// Initialize storage over an owned SQLite database facade.
    pub async fn open(db: SqliteDatabase) -> StorageResult<Self> {
        match Self::open_inner(&db).await {
            Ok(storage) => Ok(storage),
            Err(error) => {
                let _ = db.close().await;
                Err(error)
            }
        }
    }

    async fn open_inner(db: &SqliteDatabase) -> StorageResult<Self> {
        apply_sqlite_migrations(db, &sqlite_migrations()).await?;
        let next_id = db
            .get_text(
                "SELECT next_id FROM durable_metadata WHERE singleton = 1",
                vec![],
            )
            .await?
            .ok_or_else(|| StorageError::message("Durable SQLite metadata is missing"))?
            .parse::<u64>()
            .map_err(|_| StorageError::message("Durable SQLite metadata is missing"))?;
        Ok(Self {
            db: db.clone_handle(),
            next_id: Mutex::new(next_id),
            admission: Mutex::new(Admission::default()),
            drained: tokio::sync::Notify::new(),
            close_lock: tokio::sync::Mutex::new(()),
        })
    }

    fn assert_open(&self) -> StorageResult<()> {
        let admission = self
            .admission
            .lock()
            .map_err(|_| StorageError::message("SqliteStorage admission lock poisoned"))?;
        if admission.closed {
            return Err(StorageError::Closed("SqliteStorage"));
        }
        Ok(())
    }

    /// Admit one multi-query read; `close()` drains these before it closes the
    /// database (upstream `admitRead`).
    fn admit_read(&self) -> StorageResult<ReadGuard<'_>> {
        let mut admission = self
            .admission
            .lock()
            .map_err(|_| StorageError::message("SqliteStorage admission lock poisoned"))?;
        if admission.closed {
            return Err(StorageError::Closed("SqliteStorage"));
        }
        admission.admitted_reads += 1;
        Ok(ReadGuard { storage: self })
    }

    fn next_id_snapshot(&self) -> StorageResult<u64> {
        self.next_id
            .lock()
            .map(|guard| *guard)
            .map_err(|_| StorageError::message("SqliteStorage ID lock poisoned"))
    }

    fn advance_next_id(&self, candidate: u64) -> StorageResult<()> {
        let mut guard = self
            .next_id
            .lock()
            .map_err(|_| StorageError::message("SqliteStorage ID lock poisoned"))?;
        *guard = (*guard).max(candidate);
        Ok(())
    }

    async fn read_conversation(
        &self,
        id: ConversationId,
    ) -> StorageResult<Option<ConversationRecord>> {
        self.db
            .get(
                "SELECT record FROM conversations WHERE id = ?",
                vec![id.get().into()],
            )
            .await
    }

    async fn read_entry(
        &self,
        id: EntryId,
        conversation_id: Option<ConversationId>,
    ) -> StorageResult<Option<StoredEntry>> {
        let mut conversation = None;
        if let Some(conversation_id) = conversation_id {
            conversation = self.read_conversation(conversation_id).await?;
            if conversation.is_none() {
                return Err(StorageError::UnknownConversation(conversation_id));
            }
        }
        let id_value = id.get();
        let row: Option<(String, i64)> = self
            .db
            .with_connection(move |conn| {
                let mut statement =
                    conn.prepare_cached("SELECT record, commit_seq FROM entries WHERE id = ?")?;
                let mut rows = statement.query(rusqlite::params![id_value as i64])?;
                match rows.next()? {
                    None => Ok(None),
                    Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
                }
            })
            .await?;
        let Some((record, commit_seq)) = row else {
            return Ok(None);
        };
        let entry: EntryRecord = serde_json::from_str(&record)?;
        if let Some(mut conversation) = conversation {
            let mut upper = u64::MAX;
            while conversation.id != entry.conversation_id {
                let Some(parent) = conversation.parent.clone() else {
                    return Ok(None);
                };
                upper = upper.min(parent.at.get());
                conversation = self
                    .read_conversation(parent.conversation_id)
                    .await?
                    .ok_or_else(|| StorageError::UnknownConversation(parent.conversation_id))?;
            }
            if entry.id.get() > upper {
                return Ok(None);
            }
        }
        Ok(Some(StoredEntry {
            entry,
            commit_seq: Seq(commit_seq as u64),
        }))
    }

    async fn read_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
    ) -> StorageResult<Option<EntryRecord>> {
        let mut conversation = self
            .read_conversation(conversation_id)
            .await?
            .ok_or(StorageError::UnknownConversation(conversation_id))?;
        let mut upper = at_or_before_entry_id.map(EntryId::get);
        loop {
            let record: Option<EntryRecord> = match upper {
                None => {
                    self.db
                        .get(
                            "SELECT record FROM entries WHERE conversation_id = ? AND head IS NOT NULL ORDER BY id DESC LIMIT 1",
                            vec![conversation.id.get().into()],
                        )
                        .await?
                }
                Some(upper) => {
                    self.db
                        .get(
                            "SELECT record FROM entries WHERE conversation_id = ? AND head IS NOT NULL AND id <= ? ORDER BY id DESC LIMIT 1",
                            vec![conversation.id.get().into(), upper.into()],
                        )
                        .await?
                }
            };
            if record.is_some() {
                return Ok(record);
            }
            let Some(parent) = conversation.parent.clone() else {
                return Ok(None);
            };
            upper = Some(upper.map_or(parent.at.get(), |upper| upper.min(parent.at.get())));
            conversation = self
                .read_conversation(parent.conversation_id)
                .await?
                .ok_or(StorageError::UnknownConversation(parent.conversation_id))?;
        }
    }

    async fn read_entries_descending(
        &self,
        query: &EntryQuery,
        limit: usize,
        after: Option<u64>,
    ) -> StorageResult<Page<EntryRecord>> {
        let mut conversation = self
            .read_conversation(query.conversation_id)
            .await?
            .ok_or(StorageError::UnknownConversation(query.conversation_id))?;
        let mut upper = query.max_entry_id.map(EntryId::get);
        if let Some(after) = after {
            upper = Some(upper.map_or(after.saturating_sub(1), |upper| {
                upper.min(after.saturating_sub(1))
            }));
        }
        let mut values: Vec<EntryRecord> = Vec::new();
        loop {
            let mut clauses = vec!["conversation_id = ?".to_string()];
            let mut params: Vec<SqliteValue> = vec![conversation.id.get().into()];
            if let Some(min) = query.min_entry_id {
                clauses.push("id >= ?".to_string());
                params.push(min.get().into());
            }
            if let Some(upper) = upper {
                clauses.push("id <= ?".to_string());
                params.push(upper.into());
            }
            params.push((limit + 1 - values.len()).into());
            let sql = format!(
                "SELECT record FROM entries WHERE {} ORDER BY id DESC LIMIT ?",
                clauses.join(" AND ")
            );
            let rows: Vec<EntryRecord> = self.db.all(&sql, params).await?;
            values.extend(rows);
            if values.len() > limit {
                break;
            }
            let Some(parent) = conversation.parent.clone() else {
                break;
            };
            upper = Some(upper.map_or(parent.at.get(), |upper| upper.min(parent.at.get())));
            if let Some(min) = query.min_entry_id
                && upper.expect("set") < min.get()
            {
                break;
            }
            conversation = self
                .read_conversation(parent.conversation_id)
                .await?
                .ok_or(StorageError::UnknownConversation(parent.conversation_id))?;
        }
        Ok(page_values(
            values,
            limit,
            ScanOrder::Descending,
            |entry: &EntryRecord| entry.id.get(),
        ))
    }

    /// Oldest first: the fork chain's segments from the root conversation
    /// forward, each up to its fork point.
    async fn read_entries_ascending(
        &self,
        query: &EntryQuery,
        limit: usize,
        after: Option<u64>,
    ) -> StorageResult<Page<EntryRecord>> {
        let mut segments: Vec<(ConversationId, Option<u64>)> = Vec::new();
        let mut conversation = self
            .read_conversation(query.conversation_id)
            .await?
            .ok_or(StorageError::UnknownConversation(query.conversation_id))?;
        let mut upper = query.max_entry_id.map(EntryId::get);
        loop {
            segments.push((conversation.id, upper));
            let Some(parent) = conversation.parent.clone() else {
                break;
            };
            upper = Some(upper.map_or(parent.at.get(), |upper| upper.min(parent.at.get())));
            if let Some(min) = query.min_entry_id
                && upper.expect("set") < min.get()
            {
                break;
            }
            conversation = self
                .read_conversation(parent.conversation_id)
                .await?
                .ok_or(StorageError::UnknownConversation(parent.conversation_id))?;
        }
        let mut lower = query.min_entry_id.map(EntryId::get);
        if let Some(after) = after {
            lower = Some(lower.map_or(after + 1, |lower| lower.max(after + 1)));
        }
        let mut values: Vec<EntryRecord> = Vec::new();
        for (segment_id, segment_upper) in segments.into_iter().rev() {
            let mut clauses = vec!["conversation_id = ?".to_string()];
            let mut params: Vec<SqliteValue> = vec![segment_id.get().into()];
            if let Some(lower) = lower {
                clauses.push("id >= ?".to_string());
                params.push(lower.into());
            }
            if let Some(upper) = segment_upper {
                clauses.push("id <= ?".to_string());
                params.push(upper.into());
            }
            params.push((limit + 1 - values.len()).into());
            let sql = format!(
                "SELECT record FROM entries WHERE {} ORDER BY id ASC LIMIT ?",
                clauses.join(" AND ")
            );
            let rows: Vec<EntryRecord> = self.db.all(&sql, params).await?;
            values.extend(rows);
            if values.len() > limit {
                break;
            }
        }
        Ok(page_values(
            values,
            limit,
            ScanOrder::Ascending,
            |entry: &EntryRecord| entry.id.get(),
        ))
    }

    fn candidate_next_id(&self, writes: &[StorageWrite]) -> StorageResult<u64> {
        let mut next = self.next_id_snapshot()?;
        for write in writes {
            if let Some(id) = write.write_id() {
                next = next.max(id + 1);
            }
        }
        Ok(next)
    }

    async fn check_global_ids(
        &self,
        tx: &SqliteTransaction,
        writes: &[StorageWrite],
    ) -> StorageResult<()> {
        let mut claimed: std::collections::HashMap<u64, &'static str> =
            std::collections::HashMap::new();
        for write in writes {
            let Some(table) = write.table_name() else {
                continue;
            };
            let id = write.write_id().expect("table implies id");
            let existing = tx
                .get_text(
                    "SELECT record_type FROM record_ids WHERE id = ?",
                    vec![id.into()],
                )
                .await?;
            let earlier = claimed.get(&id).copied();
            if table == "conversation" || table == "entry" || table == "document" {
                if let Some(existing) = existing {
                    return Err(StorageError::message(format!(
                        "ID {id} already belongs to {existing}"
                    )));
                }
                if earlier.is_some() {
                    return Err(StorageError::message(format!(
                        "ID {id} is written more than once"
                    )));
                }
            } else {
                if let Some(existing) = existing
                    && existing != table
                {
                    return Err(StorageError::message(format!(
                        "ID {id} already belongs to {existing}"
                    )));
                }
                if let Some(earlier) = earlier
                    && earlier != table
                {
                    return Err(StorageError::message(format!(
                        "ID {id} is written as two record types"
                    )));
                }
            }
            claimed.insert(id, table);
        }
        Ok(())
    }

    fn prepare_document_actions(
        writes: &[StorageWrite],
    ) -> StorageResult<Vec<(DocumentId, DocumentAction)>> {
        let mut actions: Vec<(DocumentId, DocumentAction)> = Vec::new();
        let mut index_of: std::collections::HashMap<DocumentId, usize> =
            std::collections::HashMap::new();
        for write in writes {
            let id = match write {
                StorageWrite::DocumentCreate { record, .. }
                | StorageWrite::DocumentCopy { record, .. } => record.id,
                StorageWrite::DocumentChange { id, .. } | StorageWrite::DocumentRetire { id } => {
                    *id
                }
                _ => continue,
            };
            let position = *index_of.entry(id).or_insert_with(|| {
                actions.push((id, DocumentAction::default()));
                actions.len() - 1
            });
            let action = &mut actions[position].1;
            match write {
                StorageWrite::DocumentCreate { record, content } => {
                    if action.create.is_some() || action.content.is_some() || action.copy.is_some()
                    {
                        return Err(StorageError::message(format!(
                            "Document {id} has more than one content command"
                        )));
                    }
                    action.create = Some(record.clone());
                    action.content = Some(content.clone());
                }
                StorageWrite::DocumentCopy { record, source } => {
                    if action.create.is_some() || action.content.is_some() || action.copy.is_some()
                    {
                        return Err(StorageError::message(format!(
                            "Document {id} has more than one content command"
                        )));
                    }
                    action.create = Some(record.clone());
                    action.copy = Some(source.clone());
                }
                StorageWrite::DocumentChange { content, .. } => {
                    if action.content.is_some() || action.copy.is_some() {
                        return Err(StorageError::message(format!(
                            "Document {id} has more than one content command"
                        )));
                    }
                    action.content = Some(content.clone());
                }
                StorageWrite::DocumentRetire { .. } => {
                    if action.retire {
                        return Err(StorageError::message(format!(
                            "Document {id} is retired more than once"
                        )));
                    }
                    action.retire = true;
                }
                _ => {
                    return Err(StorageError::message(
                        "unsupported write variant in the document-action branch",
                    ));
                }
            }
        }
        Ok(actions)
    }

    async fn current_document_id(
        &self,
        tx: &SqliteTransaction,
        parts: &AddressParts,
    ) -> StorageResult<Option<DocumentId>> {
        let id = tx
            .get_i64(
                "SELECT id FROM documents WHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ? AND retired_at IS NULL LIMIT 1",
                vec![
                    parts.kind.as_str().into(),
                    parts.scope_kind.into(),
                    parts.owner_id.into(),
                    parts.family.into(),
                    parts.key_value.as_str().into(),
                ],
            )
            .await?;
        Ok(id.map(|value| id_from_number(value as u64)))
    }

    async fn check_document_actions(
        &self,
        tx: &SqliteTransaction,
        actions: &[(DocumentId, DocumentAction)],
    ) -> StorageResult<()> {
        let mut live_counts: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        for (id, action) in actions {
            if let Some(copy) = &action.copy
                && actions.iter().any(|(other, _)| other == &copy.id)
            {
                return Err(StorageError::Rejected(format!(
                    "Document copy {id} source is changed in the copy batch"
                )));
            }
            let existing: Option<DocumentRecord> = tx
                .get(
                    "SELECT record FROM documents WHERE id = ?",
                    vec![id.get().into()],
                )
                .await?;
            if action.create.is_none() && existing.is_none() {
                return Err(StorageError::message(format!("Unknown document: {id}")));
            }
            if action.create.is_some() && existing.is_some() {
                return Err(StorageError::message(format!(
                    "Document {id} already exists"
                )));
            }
            if let Some(existing) = &existing
                && existing.retired_at.is_some()
            {
                return Err(StorageError::message(format!("Document {id} is retired")));
            }
            if let Some(DocumentContent::Delta { version, .. }) = &action.content {
                let previous: Option<i64> = tx
                    .get_i64(
                        "SELECT version FROM document_revisions WHERE document_id = ? ORDER BY seq DESC LIMIT 1",
                        vec![id.get().into()],
                    )
                    .await?;
                let Some(previous) = previous else {
                    return Err(StorageError::message(format!(
                        "Document {id} delta has no base"
                    )));
                };
                if previous as u64 != *version {
                    return Err(StorageError::message(format!(
                        "Document {id} version transition requires a base"
                    )));
                }
            }
            let parts = match (&action.create, &existing) {
                (Some(create), _) => {
                    address_parts(&create.kind, &create.scope, create.key.as_deref())
                }
                (None, Some(existing)) => {
                    address_parts(&existing.kind, &existing.scope, existing.key.as_deref())
                }
                (None, None) => {
                    return Err(StorageError::message(format!(
                        "Document {id} has neither a create record nor an existing version"
                    )));
                }
            };
            let key = serde_json::to_string(&[
                parts.kind.as_str(),
                parts.scope_kind,
                &parts.owner_id.to_string(),
                &parts.family.to_string(),
                parts.key_value.as_str(),
            ])
            .unwrap_or_default();
            let current_id = self.current_document_id(tx, &parts).await?;
            let mut live = live_counts
                .get(&key)
                .copied()
                .unwrap_or(i64::from(current_id.is_some()));
            if action.retire && existing.is_some() {
                live -= 1;
            }
            if action.create.is_some() && !action.retire {
                live += 1;
            }
            live_counts.insert(key, live);
        }
        for live in live_counts.values() {
            if *live > 1 {
                return Err(StorageError::message(
                    "Document address already has a current incarnation",
                ));
            }
        }
        Ok(())
    }

    async fn apply_table_write(
        &self,
        tx: &SqliteTransaction,
        write: &StorageWrite,
        seq: Seq,
    ) -> StorageResult<()> {
        match write {
            StorageWrite::Conversation { value } => {
                self.claim_id(tx, value.id.get(), "conversation").await?;
                tx.run(
                    "INSERT INTO conversations (id, owner_conversation_id, owner_task_id, record) VALUES (?, ?, ?, ?)",
                    vec![
                        value.id.get().into(),
                        value.owner.as_ref().map(|owner| owner.conversation_id.get() as i64).into(),
                        value.owner.as_ref().map(|owner| owner.task_id.get() as i64).into(),
                        encode_json(value)?.into(),
                    ],
                )
                .await
            }
            StorageWrite::Entry { value } => {
                self.claim_id(tx, value.id.get(), "entry").await?;
                tx.run(
                    "INSERT INTO entries (id, conversation_id, head, commit_seq, record) VALUES (?, ?, ?, ?, ?)",
                    vec![
                        value.id.get().into(),
                        value.conversation_id.get().into(),
                        value.head.map(EntryId::get).into(),
                        seq.0.into(),
                        encode_json(value)?.into(),
                    ],
                )
                .await
            }
            StorageWrite::Task { value } => {
                self.claim_id(tx, value.id.get(), "task").await?;
                tx.run(
                    "INSERT INTO tasks (id, conversation_id, kind, status, abort_requested, background, record)
						VALUES (?, ?, ?, ?, ?, ?, ?)
						ON CONFLICT(id) DO UPDATE SET conversation_id = excluded.conversation_id, kind = excluded.kind,
						status = excluded.status, abort_requested = excluded.abort_requested,
						background = excluded.background, record = excluded.record",
                    vec![
                        value.id.get().into(),
                        value.conversation_id.get().into(),
                        encode_indexed_string(&value.kind).into(),
                        value.status().as_str().into(),
                        value.abort_requested.into(),
                        value.background.into(),
                        encode_json(value)?.into(),
                    ],
                )
                .await
            }
            StorageWrite::Submission { value } => {
                self.claim_id(tx, value.id().get(), "submission").await?;
                tx.run(
                    "INSERT INTO submissions (id, conversation_id, request_id, status, record) VALUES (?, ?, ?, ?, ?)
						ON CONFLICT(id) DO UPDATE SET conversation_id = excluded.conversation_id,
						request_id = excluded.request_id, status = excluded.status, record = excluded.record",
                    vec![
                        value.id().get().into(),
                        value.conversation_id().get().into(),
                        value.request_id().map(encode_indexed_string).into(),
                        value.status().into(),
                        encode_json(value)?.into(),
                    ],
                )
                .await
            }
            StorageWrite::DocumentCreate { .. }
            | StorageWrite::DocumentCopy { .. }
            | StorageWrite::DocumentChange { .. }
            | StorageWrite::DocumentRetire { .. } => Ok(()),
        }
    }

    async fn claim_id(
        &self,
        tx: &SqliteTransaction,
        id: u64,
        table: &'static str,
    ) -> StorageResult<()> {
        tx.run(
            "INSERT OR IGNORE INTO record_ids (id, record_type) VALUES (?, ?)",
            vec![id.into(), table.into()],
        )
        .await
    }

    async fn materialize_document(
        &self,
        tx: &SqliteTransaction,
        id: DocumentId,
        at: DocumentPoint,
    ) -> StorageResult<Option<StoredDocument>> {
        let row: Option<DocumentRecord> = tx
            .get(
                "SELECT record FROM documents WHERE id = ?",
                vec![id.get().into()],
            )
            .await?;
        let Some(record) = row else {
            return Ok(None);
        };
        if !at.is_current() && is_current_only(&record) {
            return Err(StorageError::message(format!(
                "Document {id} does not retain historical content"
            )));
        }
        if !is_alive_at(&record, at) {
            return Ok(None);
        }
        // Bound the revision scan by the selected sequence; `current` maps to
        // the maximum representable SQLite integer.
        let upper = at.upper().min(i64::MAX as u64) as i64;
        let id_value = id.get();
        let base: Option<(i64, String, i64, String)> = tx
            .with_connection(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT seq, kind, version, content FROM document_revisions
						WHERE document_id = ? AND kind = 'base' AND seq <= ? ORDER BY seq DESC LIMIT 1",
                )?;
                let mut rows = statement.query(rusqlite::params![id_value as i64, upper])?;
                match rows.next()? {
                    None => Ok(None),
                    Some(row) => Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))),
                }
            })
            .await?;
        let Some((base_seq, _, base_version, base_content)) = base else {
            return Err(StorageError::message(format!(
                "Document {id} is missing a required base"
            )));
        };
        let value: JsonObject = serde_json::from_str(&base_content)?;
        let tail: Vec<(i64, String, i64, String)> = tx
            .with_connection(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT seq, kind, version, content FROM document_revisions
						WHERE document_id = ? AND seq > ? AND seq <= ? ORDER BY seq",
                )?;
                let mut rows =
                    statement.query(rusqlite::params![id_value as i64, base_seq, upper])?;
                let mut values = Vec::new();
                while let Some(row) = rows.next()? {
                    values.push((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?));
                }
                Ok(values)
            })
            .await?;
        let mut ops: Vec<Vec<Op>> = Vec::new();
        for (_, kind, version, content) in &tail {
            if kind != "delta" || *version as u64 != base_version as u64 {
                return Err(StorageError::message(format!(
                    "Document {id} crosses a stored version boundary without a base"
                )));
            }
            ops.push(serde_json::from_str::<Vec<Op>>(content)?);
        }
        let value = apply_immutable_batches(
            Some(&JsonValue::Object(value)),
            ops.iter().map(Vec::as_slice),
        )?;
        let JsonValue::Object(value) = value else {
            return Err(StorageError::message(format!(
                "Document {id} root is not an object"
            )));
        };
        Ok(Some(StoredDocument {
            record,
            version: base_version as u64,
            value,
            deltas_since_base: tail.len() as u64,
        }))
    }

    async fn apply_document_actions(
        &self,
        tx: &SqliteTransaction,
        actions: &[(DocumentId, DocumentAction)],
        seq: Seq,
    ) -> StorageResult<()> {
        for (id, action) in actions {
            let id = *id;
            let mut content = action.content.clone();
            if let Some(source) = &action.copy {
                let resolved = async {
                    let stored = self.materialize_document(tx, source.id, source.at).await?;
                    let stored = stored.ok_or_else(|| {
                        StorageError::message(format!(
                            "Fork source document {} cannot be read",
                            source.id
                        ))
                    })?;
                    let create = action.create.as_ref().expect("copy carries create");
                    if !matches!(stored.record.scope, DocumentScope::Conversation { .. })
                        || !matches!(create.scope, DocumentScope::Conversation { .. })
                    {
                        return Err(StorageError::message(format!(
                            "Fork source document {} does not match the copied record",
                            source.id
                        )));
                    }
                    if stored.record.kind != create.kind
                        || stored.record.key != create.key
                        || stored.record.history != create.history
                        || stored.record.fork != create.fork
                    {
                        return Err(StorageError::message(format!(
                            "Fork source document {} does not match the copied record",
                            source.id
                        )));
                    }
                    Ok(DocumentContent::Base {
                        version: stored.version,
                        value: stored.value,
                    })
                }
                .await;
                content = Some(resolved.map_err(|error| match error {
                    StorageError::Rejected(_) => error,
                    other => {
                        StorageError::Rejected(format!("Document copy {id} was rejected: {other}"))
                    }
                })?);
            }

            let mut record: DocumentRecord;
            if let Some(create) = &action.create {
                record = DocumentRecord {
                    id,
                    kind: create.kind.clone(),
                    key: create.key.clone(),
                    created_at: seq,
                    retired_at: action.retire.then_some(seq),
                    scope: create.scope.clone(),
                    history: create.history,
                    fork: create.fork,
                };
                let parts = address_parts(&record.kind, &record.scope, record.key.as_deref());
                self.claim_id(tx, id.get(), "document").await?;
                tx.run(
                    "INSERT INTO documents (id, kind, family, key_value, scope_kind, owner_id, created_at, retired_at, record) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    vec![
                        id.get().into(),
                        parts.kind.as_str().into(),
                        parts.family.into(),
                        parts.key_value.as_str().into(),
                        parts.scope_kind.into(),
                        parts.owner_id.into(),
                        seq.0.into(),
                        action.retire.then_some(seq.0 as i64).into(),
                        encode_json(&record)?.into(),
                    ],
                )
                .await?;
            } else {
                record = tx
                    .get::<DocumentRecord>(
                        "SELECT record FROM documents WHERE id = ?",
                        vec![id.get().into()],
                    )
                    .await?
                    .ok_or_else(|| StorageError::message(format!("Unknown document: {id}")))?;
            }

            if let Some(content) = &content {
                if content.is_base() && is_current_only(&record) {
                    tx.run(
                        "DELETE FROM document_revisions WHERE document_id = ?",
                        vec![id.get().into()],
                    )
                    .await?;
                }
                let encoded = match content {
                    DocumentContent::Base { value, .. } => encode_json(value)?,
                    DocumentContent::Delta { ops, .. } => encode_json(ops)?,
                };
                let kind = if content.is_base() { "base" } else { "delta" };
                tx.run(
                    "INSERT INTO document_revisions (document_id, seq, kind, version, content) VALUES (?, ?, ?, ?, ?)",
                    vec![
                        id.get().into(),
                        seq.0.into(),
                        kind.into(),
                        content.version().into(),
                        encoded.into(),
                    ],
                )
                .await?;
            }

            if action.retire {
                if action.create.is_none() {
                    record.retired_at = Some(seq);
                    tx.run(
                        "UPDATE documents SET retired_at = ?, record = ? WHERE id = ?",
                        vec![seq.0.into(), encode_json(&record)?.into(), id.get().into()],
                    )
                    .await?;
                }
                if is_current_only(&record) {
                    tx.run(
                        "DELETE FROM document_revisions WHERE document_id = ?",
                        vec![id.get().into()],
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn commit(&self, writes: &[StorageWrite], _context: &Context) -> StorageResult<Seq> {
        self.assert_open()?;
        let actions = Self::prepare_document_actions(writes)?;
        let candidate_next_id = self.candidate_next_id(writes)?;
        let seq = self
            .db
            .transaction(move |tx| async move {
                let metadata_next_id = tx
                    .get_text(
                        "SELECT next_id FROM durable_metadata WHERE singleton = 1",
                        vec![],
                    )
                    .await?
                    .ok_or_else(|| StorageError::message("Durable SQLite metadata is missing"))?;
                let committed_seq = tx
                    .get_i64(
                        "SELECT next_seq FROM durable_metadata WHERE singleton = 1",
                        vec![],
                    )
                    .await?
                    .ok_or_else(|| StorageError::message("Durable SQLite metadata is missing"))?
                    as u64;
                let committed_seq = Seq(committed_seq);
                self.check_global_ids(&tx, writes).await?;
                self.check_document_actions(&tx, &actions).await?;
                for write in writes {
                    self.apply_table_write(&tx, write, committed_seq).await?;
                }
                self.apply_document_actions(&tx, &actions, committed_seq)
                    .await?;
                let current_next_id = metadata_next_id
                    .parse::<u64>()
                    .map_err(|_| StorageError::message("Durable SQLite metadata is missing"))?;
                tx.run(
                    "UPDATE durable_metadata SET next_id = ?, next_seq = ? WHERE singleton = 1",
                    vec![
                        current_next_id.max(candidate_next_id).to_string().into(),
                        (committed_seq.0 + 1).into(),
                    ],
                )
                .await?;
                Ok(committed_seq)
            })
            .await?;
        self.advance_next_id(candidate_next_id)?;
        Ok(seq)
    }

    async fn mint_id(&self) -> StorageResult<u64> {
        self.assert_open()?;
        let mut guard = self
            .next_id
            .lock()
            .map_err(|_| StorageError::message("SqliteStorage ID lock poisoned"))?;
        let id = *guard;
        if id > MAX_SAFE_INTEGER {
            return Err(StorageError::IdSpaceExhausted);
        }
        *guard += 1;
        Ok(id)
    }

    async fn conversation(
        &self,
        id: ConversationId,
        _context: &Context,
    ) -> StorageResult<Option<ConversationRecord>> {
        self.assert_open()?;
        self.read_conversation(id).await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: &Context,
    ) -> StorageResult<Page<ConversationRecord>> {
        self.assert_open()?;
        let start = scan_start(query.order, cursor.as_ref(), ScanOrder::Ascending)?;
        let (clause, param, direction) = scan_sql(start);
        let mut clauses = vec![clause.to_string()];
        let mut params: Vec<SqliteValue> = vec![param.into()];
        if let Some(owner) = query.owner_conversation_id {
            clauses.push("owner_conversation_id = ?".to_string());
            params.push(owner.get().into());
        }
        if let Some(task) = query.owner_task_id {
            clauses.push("owner_task_id = ?".to_string());
            params.push(task.get().into());
        }
        params.push((limit + 1).into());
        let sql = format!(
            "SELECT record FROM conversations WHERE {} ORDER BY id {} LIMIT ?",
            clauses.join(" AND "),
            direction
        );
        let rows: Vec<ConversationRecord> = self.db.all(&sql, params).await?;
        Ok(page_values(
            rows,
            limit,
            start.order,
            |record: &ConversationRecord| record.id.get(),
        ))
    }

    async fn entry(&self, id: EntryId, _context: &Context) -> StorageResult<Option<StoredEntry>> {
        let _guard = self.admit_read()?;
        self.read_entry(id, None).await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        _context: &Context,
    ) -> StorageResult<Option<StoredEntry>> {
        let _guard = self.admit_read()?;
        self.read_entry(id, Some(conversation_id)).await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        _context: &Context,
    ) -> StorageResult<Option<EntryRecord>> {
        let _guard = self.admit_read()?;
        self.read_latest_head_marker(conversation_id, at_or_before_entry_id)
            .await
    }

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: &Context,
    ) -> StorageResult<Page<EntryRecord>> {
        let _guard = self.admit_read()?;
        let start = scan_start(query.order, cursor.as_ref(), ScanOrder::Descending)?;
        if start.order == ScanOrder::Ascending {
            self.read_entries_ascending(&query, limit, start.after)
                .await
        } else {
            self.read_entries_descending(&query, limit, start.after)
                .await
        }
    }

    async fn task(&self, id: TaskId, _context: &Context) -> StorageResult<Option<TaskRecord>> {
        self.assert_open()?;
        self.db
            .get(
                "SELECT record FROM tasks WHERE id = ?",
                vec![id.get().into()],
            )
            .await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: &Context,
    ) -> StorageResult<Page<TaskRecord>> {
        self.assert_open()?;
        let start = scan_start(query.order, cursor.as_ref(), ScanOrder::Ascending)?;
        let (clause, param, direction) = scan_sql(start);
        let mut clauses = vec![clause.to_string()];
        let mut params: Vec<SqliteValue> = vec![param.into()];
        if let Some(conversation) = query.conversation_id {
            clauses.push("conversation_id = ?".to_string());
            params.push(conversation.get().into());
        }
        if let Some(kind) = &query.kind {
            clauses.push("kind = ?".to_string());
            params.push(encode_indexed_string(kind).into());
        }
        if let Some(status) = query.status {
            clauses.push("status = ?".to_string());
            params.push(status.as_str().into());
        }
        if let Some(abort_requested) = query.abort_requested {
            clauses.push("abort_requested = ?".to_string());
            params.push(abort_requested.into());
        }
        if let Some(background) = query.background {
            clauses.push("background = ?".to_string());
            params.push(background.into());
        }
        params.push((limit + 1).into());
        let sql = format!(
            "SELECT record FROM tasks WHERE {} ORDER BY id {} LIMIT ?",
            clauses.join(" AND "),
            direction
        );
        let rows: Vec<TaskRecord> = self.db.all(&sql, params).await?;
        Ok(page_values(
            rows,
            limit,
            start.order,
            |task: &TaskRecord| task.id.get(),
        ))
    }

    async fn submission(
        &self,
        id: SubmissionId,
        _context: &Context,
    ) -> StorageResult<Option<SubmissionRecord>> {
        self.assert_open()?;
        self.db
            .get(
                "SELECT record FROM submissions WHERE id = ?",
                vec![id.get().into()],
            )
            .await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: &Context,
    ) -> StorageResult<Page<SubmissionRecord>> {
        self.assert_open()?;
        let start = scan_start(query.order, cursor.as_ref(), ScanOrder::Ascending)?;
        let (clause, param, direction) = scan_sql(start);
        let mut clauses = vec![clause.to_string()];
        let mut params: Vec<SqliteValue> = vec![param.into()];
        if let Some(conversation) = query.conversation_id {
            clauses.push("conversation_id = ?".to_string());
            params.push(conversation.get().into());
        }
        if let Some(status) = query.status {
            clauses.push("status = ?".to_string());
            params.push(status.as_str().into());
        }
        params.push((limit + 1).into());
        let sql = format!(
            "SELECT record FROM submissions WHERE {} ORDER BY id {} LIMIT ?",
            clauses.join(" AND "),
            direction
        );
        let rows: Vec<SubmissionRecord> = self.db.all(&sql, params).await?;
        Ok(page_values(
            rows,
            limit,
            start.order,
            |record: &SubmissionRecord| record.id().get(),
        ))
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        _context: &Context,
    ) -> StorageResult<Option<SubmissionRecord>> {
        self.assert_open()?;
        self.db
            .get(
                "SELECT record FROM submissions WHERE conversation_id = ? AND request_id = ?",
                vec![
                    conversation_id.get().into(),
                    encode_indexed_string(request_id).into(),
                ],
            )
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        _context: &Context,
    ) -> StorageResult<Option<DocumentRecord>> {
        self.assert_open()?;
        let parts = address_parts_of(&address);
        let params: Vec<SqliteValue> = vec![
            parts.kind.as_str().into(),
            parts.scope_kind.into(),
            parts.owner_id.into(),
            parts.family.into(),
            parts.key_value.as_str().into(),
        ];
        let sql = if at.is_current() {
            "SELECT record FROM documents
				WHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ?
				AND retired_at IS NULL ORDER BY created_at DESC LIMIT 1"
        } else {
            "SELECT record FROM documents
				WHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ?
				AND created_at <= ? AND (retired_at IS NULL OR retired_at > ?)
				ORDER BY created_at DESC LIMIT 1"
        };
        if at.is_current() {
            self.db.get(sql, params).await
        } else {
            let bound = at.upper();
            let mut params = params;
            params.push(bound.into());
            params.push(bound.into());
            self.db.get(sql, params).await
        }
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        _context: &Context,
    ) -> StorageResult<Option<StoredDocument>> {
        self.assert_open()?;
        // The record and revision queries must observe one committed state; a
        // commit between them can replace the base.
        self.db
            .transaction(move |tx| async move { self.materialize_document(&tx, id, at).await })
            .await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: &Context,
    ) -> StorageResult<Page<DocumentRecord>> {
        self.assert_open()?;
        crate::storage::validate_cursor(cursor.as_ref())?;
        let scope = scope_columns(&query.scope);
        let mut clauses = vec![
            "scope_kind = ?".to_string(),
            "owner_id = ?".to_string(),
            "id > ?".to_string(),
        ];
        let cursor_after = cursor
            .as_ref()
            .map(|cursor| cursor.after as i64)
            .unwrap_or(-1);
        let mut params: Vec<SqliteValue> = vec![
            scope.scope_kind.into(),
            scope.owner_id.into(),
            cursor_after.into(),
        ];
        if let Some(kind) = &query.kind {
            clauses.push("kind = ?".to_string());
            params.push(encode_indexed_string(kind).into());
        }
        if query.at.is_current() {
            clauses.push("retired_at IS NULL".to_string());
        } else {
            clauses.push("created_at <= ?".to_string());
            clauses.push("(retired_at IS NULL OR retired_at > ?)".to_string());
            params.push(query.at.upper().into());
            params.push(query.at.upper().into());
        }
        params.push((limit + 1).into());
        let sql = format!(
            "SELECT record FROM documents WHERE {} ORDER BY id LIMIT ?",
            clauses.join(" AND ")
        );
        let rows: Vec<DocumentRecord> = self.db.all(&sql, params).await?;
        Ok(page_values(
            rows,
            limit,
            ScanOrder::Ascending,
            |record: &DocumentRecord| record.id.get(),
        ))
    }

    async fn close(&self, _context: &Context) -> StorageResult<()> {
        // Repeated closes coalesce: a second caller settles only after the
        // first finished closing the database (upstream shares one promise).
        let _close_guard = self.close_lock.lock().await;
        {
            let mut admission = self
                .admission
                .lock()
                .map_err(|_| StorageError::message("SqliteStorage admission lock poisoned"))?;
            admission.closed = true;
        }
        loop {
            let notified = self.drained.notified();
            let admitted = self
                .admission
                .lock()
                .map_err(|_| StorageError::message("SqliteStorage admission lock poisoned"))?
                .admitted_reads;
            if admitted == 0 {
                break;
            }
            notified.await;
        }
        self.db.close().await
    }
}
