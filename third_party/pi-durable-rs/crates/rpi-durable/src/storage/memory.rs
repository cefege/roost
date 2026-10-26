//! Detached in-memory reference implementation of [`Storage`]
//! (port of `src/storage/memory.ts` @ `abe508e1b`).
//!
//! Reads and retained writes are cloned intentionally to match the ownership
//! boundary of serialization-backed stores. This is backend conformance, not
//! validation.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::chord::context::Context;
use crate::chord::delta::{Op, apply_immutable_batches};
use crate::chord::json::JsonValue;
use crate::errors::{StorageError, StorageResult};
use crate::ids::{ConversationId, DocumentId, EntryId, Seq, SubmissionId, TaskId, seq_from_number};
use crate::storage::scan::{scan_indexes, scan_start};
use crate::storage::{
    address_key, insert_map_id, insert_sorted, is_alive_at, is_current_only,
    is_current_only_create, page_values, record_address_key, record_address_key_of, remove_sorted,
    scope_key,
};
use crate::types::{
    ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentContent,
    DocumentCreate, DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope, EntryQuery,
    EntryRecord, Page, ScanOrder, Storage, StorageWrite, StoredDocument, SubmissionQuery,
    SubmissionRecord, SubmissionStatusFilter, TaskQuery, TaskRecord, TaskStatus,
};

type StoredTask = TaskRecord;
type TableName = &'static str;

#[derive(Debug, Clone)]
struct DocumentRevision {
    content: DocumentContent,
    seq: Seq,
}

#[derive(Debug, Clone)]
struct StoredDocumentState {
    record: DocumentRecord,
    revisions: Vec<DocumentRevision>,
}

#[derive(Debug, Clone, Default)]
struct DocumentAction {
    create: Option<DocumentCreate>,
    content: Option<DocumentContent>,
    retire: bool,
}

#[derive(Debug, Clone, Default)]
struct DocumentAddressIndex {
    ids: Vec<DocumentId>,
    current_id: Option<DocumentId>,
}

#[derive(Debug, Default)]
struct State {
    record_types: HashMap<u64, TableName>,
    conversations: HashMap<ConversationId, ConversationRecord>,
    conversation_ids: Vec<ConversationId>,
    conversation_ids_by_owner_conversation: HashMap<ConversationId, Vec<ConversationId>>,
    conversation_ids_by_owner_task: HashMap<TaskId, Vec<ConversationId>>,
    entries: HashMap<EntryId, EntryRecord>,
    entry_ids: HashMap<ConversationId, Vec<EntryId>>,
    head_entry_ids: HashMap<ConversationId, Vec<EntryId>>,
    entry_commit_seqs: HashMap<EntryId, Seq>,
    tasks: HashMap<TaskId, StoredTask>,
    task_ids: Vec<TaskId>,
    task_ids_by_status: HashMap<TaskStatus, Vec<TaskId>>,
    submissions: HashMap<SubmissionId, SubmissionRecord>,
    submission_ids: Vec<SubmissionId>,
    submission_ids_by_status: HashMap<SubmissionStatusFilter, Vec<SubmissionId>>,
    submission_ids_by_request: HashMap<ConversationId, HashMap<String, SubmissionId>>,
    documents: HashMap<DocumentId, StoredDocumentState>,
    document_addresses: HashMap<String, DocumentAddressIndex>,
    document_ids_by_scope: HashMap<String, Vec<DocumentId>>,
}

#[derive(Debug)]
struct Inner {
    state: State,
    next_id: u64,
    next_seq: u64,
    closed: bool,
}

/// A fully validated, detached state mutation whose application performs no
/// fallible preparation.
pub struct PreparedMemoryCommit {
    seq: Seq,
    writes: Vec<StorageWrite>,
    actions: Vec<(DocumentId, DocumentAction)>,
    applied: bool,
}

impl PreparedMemoryCommit {
    /// Commit sequence.
    pub fn seq(&self) -> Seq {
        self.seq
    }

    /// Deeply detached writes for persistence (document copies already resolved).
    pub fn writes(&self) -> &[StorageWrite] {
        &self.writes
    }
}

/// Detached in-memory reference implementation of [`Storage`].
pub struct MemoryStorage {
    inner: Mutex<Inner>,
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStorage {
    /// Create an empty storage.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                state: State::default(),
                next_id: 2,
                next_seq: 1,
                closed: false,
            }),
        }
    }

    fn lock(&self) -> StorageResult<std::sync::MutexGuard<'_, Inner>> {
        self.inner
            .lock()
            .map_err(|_| StorageError::message("MemoryStorage lock poisoned"))
    }

    fn assert_open(inner: &Inner) -> StorageResult<()> {
        if inner.closed {
            return Err(StorageError::Closed("MemoryStorage"));
        }
        Ok(())
    }

    /// Validate and detach one commit without changing observable state.
    pub fn prepare_commit(
        &self,
        writes: Vec<StorageWrite>,
        seq: Option<Seq>,
    ) -> StorageResult<PreparedMemoryCommit> {
        let mut inner = self.lock()?;
        prepare(&mut inner, writes, seq)
    }

    /// Apply a detached prepared commit. Calling it twice returns the same
    /// sequence without re-applying (upstream `PreparedMemoryCommit.apply`).
    pub fn apply_prepared(&self, prepared: &mut PreparedMemoryCommit) -> StorageResult<Seq> {
        let mut inner = self.lock()?;
        if prepared.applied {
            return Ok(prepared.seq);
        }
        let seq = apply_prepared_inner(&mut inner, prepared)?;
        Ok(seq)
    }
}

fn prepare(
    inner: &mut Inner,
    writes: Vec<StorageWrite>,
    seq: Option<Seq>,
) -> StorageResult<PreparedMemoryCommit> {
    MemoryStorage::assert_open(inner)?;
    let seq = seq.unwrap_or_else(|| seq_from_number(inner.next_seq));
    if seq.0 < inner.next_seq {
        return Err(StorageError::message(format!(
            "Commit sequence {seq} does not strictly increase"
        )));
    }
    let writes = resolve_document_copies(inner, writes)?;
    check_global_ids(&inner.state, &writes)?;
    let actions = prepare_document_actions(&writes)?;
    check_document_actions(&inner.state, &actions)?;
    Ok(PreparedMemoryCommit {
        seq,
        writes,
        actions,
        applied: false,
    })
}

fn apply_prepared_inner(
    inner: &mut Inner,
    prepared: &mut PreparedMemoryCommit,
) -> StorageResult<Seq> {
    MemoryStorage::assert_open(inner)?;
    if prepared.applied {
        return Ok(prepared.seq);
    }
    let seq = prepared.seq;
    for write in &prepared.writes {
        match write {
            StorageWrite::Conversation { value } => {
                inner
                    .state
                    .record_types
                    .insert(value.id.get(), "conversation");
                inner.state.conversations.insert(value.id, value.clone());
                insert_sorted(&mut inner.state.conversation_ids, value.id);
                if let Some(owner) = &value.owner {
                    insert_map_id(
                        &mut inner.state.conversation_ids_by_owner_conversation,
                        owner.conversation_id,
                        value.id,
                    );
                    insert_map_id(
                        &mut inner.state.conversation_ids_by_owner_task,
                        owner.task_id,
                        value.id,
                    );
                }
                inner.next_id = inner.next_id.max(value.id.get() + 1);
            }
            StorageWrite::Entry { value } => {
                inner.state.record_types.insert(value.id.get(), "entry");
                inner.state.entries.insert(value.id, value.clone());
                inner.state.entry_commit_seqs.insert(value.id, seq);
                insert_sorted(
                    inner
                        .state
                        .entry_ids
                        .entry(value.conversation_id)
                        .or_default(),
                    value.id,
                );
                if value.head.is_some() {
                    insert_sorted(
                        inner
                            .state
                            .head_entry_ids
                            .entry(value.conversation_id)
                            .or_default(),
                        value.id,
                    );
                }
                inner.next_id = inner.next_id.max(value.id.get() + 1);
            }
            StorageWrite::Task { value } => {
                inner.state.record_types.insert(value.id.get(), "task");
                let previous = inner
                    .state
                    .tasks
                    .get(&value.id)
                    .map(|record| record.status());
                if previous.is_none() {
                    insert_sorted(&mut inner.state.task_ids, value.id);
                    insert_sorted(
                        inner
                            .state
                            .task_ids_by_status
                            .entry(value.status())
                            .or_default(),
                        value.id,
                    );
                } else if previous != Some(value.status()) {
                    if let Some(previous) = previous
                        && let Some(ids) = inner.state.task_ids_by_status.get_mut(&previous)
                    {
                        remove_sorted(ids, value.id);
                    }
                    insert_sorted(
                        inner
                            .state
                            .task_ids_by_status
                            .entry(value.status())
                            .or_default(),
                        value.id,
                    );
                }
                inner.state.tasks.insert(value.id, value.clone());
                inner.next_id = inner.next_id.max(value.id.get() + 1);
            }
            StorageWrite::Submission { value } => {
                inner
                    .state
                    .record_types
                    .insert(value.id().get(), "submission");
                let previous = inner.state.submissions.get(&value.id()).cloned();
                let current_status = submission_status(value);
                if let Some(previous) = &previous {
                    let previous_status = submission_status(previous);
                    if previous_status != current_status
                        && let Some(ids) = inner
                            .state
                            .submission_ids_by_status
                            .get_mut(&previous_status)
                    {
                        remove_sorted(ids, value.id());
                    }
                } else {
                    insert_sorted(&mut inner.state.submission_ids, value.id());
                }
                insert_sorted(
                    inner
                        .state
                        .submission_ids_by_status
                        .entry(current_status)
                        .or_default(),
                    value.id(),
                );
                if let Some(previous) = &previous
                    && let Some(request_id) = previous.request_id()
                {
                    let conversation = previous.conversation_id();
                    let remove_conversation = if let Some(requests) =
                        inner.state.submission_ids_by_request.get_mut(&conversation)
                    {
                        if requests.get(request_id) == Some(&value.id()) {
                            requests.remove(request_id);
                        }
                        requests.is_empty()
                    } else {
                        false
                    };
                    if remove_conversation {
                        inner.state.submission_ids_by_request.remove(&conversation);
                    }
                }
                inner.state.submissions.insert(value.id(), value.clone());
                if let Some(request_id) = value.request_id() {
                    inner
                        .state
                        .submission_ids_by_request
                        .entry(value.conversation_id())
                        .or_default()
                        .insert(request_id.to_string(), value.id());
                }
                inner.next_id = inner.next_id.max(value.id().get() + 1);
            }
            StorageWrite::DocumentCopy { .. } => {
                return Err(StorageError::message(
                    "Prepared document copy was not resolved",
                ));
            }
            StorageWrite::DocumentCreate { .. }
            | StorageWrite::DocumentChange { .. }
            | StorageWrite::DocumentRetire { .. } => {}
        }
    }
    apply_document_actions(&mut inner.state, &prepared.actions, seq);
    for (id, action) in &prepared.actions {
        if action.create.is_some() {
            inner.next_id = inner.next_id.max(id.get() + 1);
        }
    }
    inner.next_seq = seq.0 + 1;
    prepared.applied = true;
    Ok(seq)
}

fn submission_status(record: &SubmissionRecord) -> SubmissionStatusFilter {
    match record.status() {
        "queued" => SubmissionStatusFilter::Queued,
        "placed" => SubmissionStatusFilter::Placed,
        "done" => SubmissionStatusFilter::Done,
        _ => SubmissionStatusFilter::Unanswered,
    }
}

fn resolve_document_copies(
    inner: &Inner,
    writes: Vec<StorageWrite>,
) -> StorageResult<Vec<StorageWrite>> {
    if !writes
        .iter()
        .any(|write| matches!(write, StorageWrite::DocumentCopy { .. }))
    {
        return Ok(writes);
    }
    let mut changed_document_ids = std::collections::HashSet::new();
    for write in &writes {
        match write {
            StorageWrite::DocumentCreate { record, .. }
            | StorageWrite::DocumentCopy { record, .. } => {
                changed_document_ids.insert(record.id);
            }
            StorageWrite::DocumentChange { id, .. } | StorageWrite::DocumentRetire { id } => {
                changed_document_ids.insert(*id);
            }
            _ => {}
        }
    }
    writes
        .into_iter()
        .map(|write| {
            let StorageWrite::DocumentCopy { record, source } = write else {
                return Ok(write);
            };
            let resolve = || -> StorageResult<StorageWrite> {
                if changed_document_ids.contains(&source.id) {
                    return Err(StorageError::message(format!(
                        "Fork source document {} is changed in the copy batch",
                        source.id
                    )));
                }
                let stored =
                    materialize_document(&inner.state, source.id, source.at)?.ok_or_else(|| {
                        StorageError::message(format!(
                            "Fork source document {} cannot be read",
                            source.id
                        ))
                    })?;
                if !matches!(stored.record.scope, DocumentScope::Conversation { .. })
                    || !matches!(record.scope, DocumentScope::Conversation { .. })
                {
                    return Err(StorageError::message(format!(
                        "Fork source document {} does not match the copied record",
                        source.id
                    )));
                }
                if stored.record.kind != record.kind
                    || stored.record.key != record.key
                    || stored.record.history != record.history
                    || stored.record.fork != record.fork
                {
                    return Err(StorageError::message(format!(
                        "Fork source document {} does not match the copied record",
                        source.id
                    )));
                }
                Ok(StorageWrite::DocumentCreate {
                    record,
                    content: DocumentContent::Base {
                        version: stored.version,
                        value: stored.value,
                    },
                })
            };
            resolve().map_err(|error| match error {
                StorageError::Rejected(_) => error,
                other => StorageError::Rejected(format!(
                    "Document copy {} was rejected: {other}",
                    source.id
                )),
            })
        })
        .collect()
}

fn check_global_ids(state: &State, writes: &[StorageWrite]) -> StorageResult<()> {
    let mut claimed: HashMap<u64, TableName> = HashMap::new();
    for write in writes {
        let Some(table) = write.table_name() else {
            continue;
        };
        let id = write.write_id().expect("write table implies id");
        let existing = state.record_types.get(&id).copied();
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
    let mut index_of: HashMap<DocumentId, usize> = HashMap::new();
    for write in writes {
        let id = match write {
            StorageWrite::DocumentCreate { record, .. }
            | StorageWrite::DocumentCopy { record, .. } => record.id,
            StorageWrite::DocumentChange { id, .. } | StorageWrite::DocumentRetire { id } => *id,
            _ => continue,
        };
        let position = *index_of.entry(id).or_insert_with(|| {
            actions.push((id, DocumentAction::default()));
            actions.len() - 1
        });
        let action = &mut actions[position].1;
        match write {
            StorageWrite::DocumentCreate { record, content } => {
                if action.create.is_some() || action.content.is_some() {
                    return Err(StorageError::message(format!(
                        "Document {id} has more than one content command"
                    )));
                }
                action.create = Some(record.clone());
                action.content = Some(content.clone());
            }
            StorageWrite::DocumentCopy { record, .. } => {
                if action.create.is_some() || action.content.is_some() {
                    return Err(StorageError::message(format!(
                        "Document {id} has more than one content command"
                    )));
                }
                action.create = Some(record.clone());
            }
            StorageWrite::DocumentChange { content, .. } => {
                if action.content.is_some() {
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

fn check_document_actions(
    state: &State,
    actions: &[(DocumentId, DocumentAction)],
) -> StorageResult<()> {
    let mut live_counts: HashMap<String, i64> = HashMap::new();
    for (id, action) in actions {
        let existing = state.documents.get(id);
        if action.create.is_none() && existing.is_none() {
            return Err(StorageError::message(format!("Unknown document: {id}")));
        }
        if action.create.is_some() && existing.is_some() {
            return Err(StorageError::message(format!(
                "Document {id} already exists"
            )));
        }
        if let Some(existing) = existing
            && existing.record.retired_at.is_some()
        {
            return Err(StorageError::message(format!("Document {id} is retired")));
        }
        let previous = existing.and_then(|stored| stored.revisions.last());
        if let Some(DocumentContent::Delta { version, .. }) = &action.content {
            let Some(previous) = previous else {
                return Err(StorageError::message(format!(
                    "Document {id} delta has no base"
                )));
            };
            if previous.content.version() != *version {
                return Err(StorageError::message(format!(
                    "Document {id} version transition requires a base"
                )));
            }
        }

        let key = match &action.create {
            None => record_address_key_of(&existing.expect("checked above").record),
            Some(create) => record_address_key(create),
        };
        let current_id = state
            .document_addresses
            .get(&key)
            .and_then(|address| address.current_id);
        let mut live = live_counts
            .get(&key)
            .copied()
            .unwrap_or(i64::from(current_id.is_some()));
        if action.retire && current_id == Some(*id) {
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

fn apply_document_actions(state: &mut State, actions: &[(DocumentId, DocumentAction)], seq: Seq) {
    for (id, action) in actions {
        let id = *id;
        if let Some(create) = &action.create {
            let record = DocumentRecord {
                id: create.id,
                kind: create.kind.clone(),
                key: create.key.clone(),
                created_at: seq,
                retired_at: action.retire.then_some(seq),
                scope: create.scope.clone(),
                history: create.history,
                fork: create.fork,
            };
            let content = action
                .content
                .clone()
                .expect("create always carries content");
            state.record_types.insert(id.get(), "document");
            state.documents.insert(
                id,
                StoredDocumentState {
                    record: record.clone(),
                    revisions: vec![DocumentRevision { content, seq }],
                },
            );

            let key = record_address_key_of(&record);
            let address = state.document_addresses.entry(key.clone()).or_default();
            insert_sorted(&mut address.ids, id);

            let scope_ids = state
                .document_ids_by_scope
                .entry(scope_key(&record.scope))
                .or_default();
            insert_sorted(scope_ids, id);
        } else if let Some(content) = &action.content {
            let stored = state.documents.get_mut(&id).expect("checked above");
            if content.is_base() && is_current_only(&stored.record) {
                stored.revisions = vec![DocumentRevision {
                    content: content.clone(),
                    seq,
                }];
            } else {
                stored.revisions.push(DocumentRevision {
                    content: content.clone(),
                    seq,
                });
            }
        }

        if action.retire && action.create.is_none() {
            let stored = state.documents.get_mut(&id).expect("checked above");
            stored.record.retired_at = Some(seq);
            if is_current_only(&stored.record) {
                stored.revisions.clear();
            }
        }
        if let Some(create) = &action.create
            && action.retire
            && is_current_only_create(create)
        {
            let stored = state.documents.get_mut(&id).expect("checked above");
            stored.revisions.clear();
        }
        if action.create.is_some() || action.retire {
            let stored = state.documents.get(&id).expect("checked above");
            let key = record_address_key_of(&stored.record);
            let address = state
                .document_addresses
                .get_mut(&key)
                .expect("create indexed the address");
            if action.retire && address.current_id == Some(id) {
                address.current_id = None;
            }
            if action.create.is_some() && !action.retire {
                address.current_id = Some(id);
            }
        }
    }
}

fn materialize_document(
    state: &State,
    id: DocumentId,
    at: DocumentPoint,
) -> StorageResult<Option<StoredDocument>> {
    let Some(stored) = state.documents.get(&id) else {
        return Ok(None);
    };
    if !at.is_current() && is_current_only(&stored.record) {
        return Err(StorageError::message(format!(
            "Document {id} does not retain historical content"
        )));
    }
    if !is_alive_at(&stored.record, at) {
        return Ok(None);
    }
    let revisions: Vec<&DocumentRevision> = match at {
        DocumentPoint::Current(_) => stored.revisions.iter().collect(),
        DocumentPoint::At(seq) => stored
            .revisions
            .iter()
            .filter(|revision| revision.seq <= seq)
            .collect(),
    };
    let Some(base_index) = revisions
        .iter()
        .rposition(|revision| revision.content.is_base())
    else {
        return Err(StorageError::message(format!(
            "Document {id} is missing a required base"
        )));
    };
    let base = revisions[base_index];
    let version = base.content.version();
    let DocumentContent::Base {
        value: base_value, ..
    } = &base.content
    else {
        return Err(StorageError::message(format!(
            "Document {id} base revision is not a base"
        )));
    };
    let base_value = base_value.clone();
    let mut batches: Vec<&[Op]> = Vec::new();
    let mut owned: Vec<Vec<Op>> = Vec::new();
    for revision in &revisions[base_index + 1..] {
        match &revision.content {
            DocumentContent::Delta {
                version: delta_version,
                ops,
            } => {
                if *delta_version != version {
                    return Err(StorageError::message(format!(
                        "Document {id} crosses a stored version boundary without a base"
                    )));
                }
                owned.push(ops.clone());
            }
            DocumentContent::Base { .. } => {
                return Err(StorageError::message(format!(
                    "Document {id} crosses a stored version boundary without a base"
                )));
            }
        }
    }
    batches.extend(owned.iter().map(Vec::as_slice));
    let value = apply_immutable_batches(Some(&JsonValue::Object(base_value)), batches)?;
    let JsonValue::Object(value) = value else {
        return Err(StorageError::message(format!(
            "Document {id} root is not an object"
        )));
    };
    Ok(Some(StoredDocument {
        record: stored.record.clone(),
        version,
        value,
        deltas_since_base: (revisions.len() - base_index - 1) as u64,
    }))
}

fn visible_entries(
    state: &State,
    conversation_id: ConversationId,
    min_entry_id: Option<u64>,
    max_entry_id: Option<u64>,
) -> StorageResult<Vec<EntryRecord>> {
    if !state.conversations.contains_key(&conversation_id) {
        return Err(StorageError::UnknownConversation(conversation_id));
    }
    let mut values = Vec::new();
    let mut current_id = conversation_id;
    let mut upper = max_entry_id.unwrap_or(u64::MAX);
    loop {
        let empty = Vec::new();
        let ids = state.entry_ids.get(&current_id).unwrap_or(&empty);
        let end = ids.partition_point(|id| id.get() <= upper);
        for index in (0..end).rev() {
            let id = ids[index];
            if let Some(min) = min_entry_id
                && id.get() < min
            {
                break;
            }
            values.push(state.entries.get(&id).expect("indexed entry").clone());
        }
        let conversation = state.conversations.get(&current_id).expect("checked above");
        let Some(parent) = &conversation.parent else {
            break;
        };
        upper = upper.min(parent.at.get());
        if let Some(min) = min_entry_id
            && upper < min
        {
            break;
        }
        current_id = parent.conversation_id;
    }
    Ok(values)
}

fn visible_entries_ascending(
    state: &State,
    conversation_id: ConversationId,
    min_entry_id: Option<u64>,
    max_entry_id: Option<u64>,
) -> StorageResult<Vec<EntryRecord>> {
    if !state.conversations.contains_key(&conversation_id) {
        return Err(StorageError::UnknownConversation(conversation_id));
    }
    let mut segments: Vec<(ConversationId, u64)> = Vec::new();
    let mut current_id = conversation_id;
    let mut upper = max_entry_id.unwrap_or(u64::MAX);
    loop {
        segments.push((current_id, upper));
        let conversation = state.conversations.get(&current_id).expect("checked above");
        let Some(parent) = &conversation.parent else {
            break;
        };
        upper = upper.min(parent.at.get());
        if let Some(min) = min_entry_id
            && upper < min
        {
            break;
        }
        current_id = parent.conversation_id;
    }
    let mut values = Vec::new();
    for (segment_id, segment_upper) in segments.into_iter().rev() {
        let empty = Vec::new();
        let ids = state.entry_ids.get(&segment_id).unwrap_or(&empty);
        let start = match min_entry_id {
            None => 0,
            Some(min) => ids.partition_point(|id| id.get() < min),
        };
        for id in &ids[start..] {
            if id.get() > segment_upper {
                break;
            }
            values.push(state.entries.get(id).expect("indexed entry").clone());
        }
    }
    Ok(values)
}

#[async_trait]
impl Storage for MemoryStorage {
    async fn commit(&self, writes: &[StorageWrite], _context: &Context) -> StorageResult<Seq> {
        let mut inner = self.lock()?;
        let mut prepared = prepare(&mut inner, writes.to_vec(), None)?;
        apply_prepared_inner(&mut inner, &mut prepared)
    }

    async fn mint_id(&self) -> StorageResult<u64> {
        let mut inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        let id = inner.next_id;
        if id > crate::storage::MAX_SAFE_INTEGER {
            return Err(StorageError::IdSpaceExhausted);
        }
        inner.next_id += 1;
        Ok(id)
    }

    async fn conversation(
        &self,
        id: ConversationId,
        _context: &Context,
    ) -> StorageResult<Option<ConversationRecord>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        Ok(inner.state.conversations.get(&id).cloned())
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: &Context,
    ) -> StorageResult<Page<ConversationRecord>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        let ids: Vec<ConversationId> = if let Some(task) = query.owner_task_id {
            inner
                .state
                .conversation_ids_by_owner_task
                .get(&task)
                .cloned()
                .unwrap_or_default()
        } else if let Some(owner) = query.owner_conversation_id {
            inner
                .state
                .conversation_ids_by_owner_conversation
                .get(&owner)
                .cloned()
                .unwrap_or_default()
        } else {
            inner.state.conversation_ids.clone()
        };
        let start = scan_start(query.order, cursor.as_ref(), ScanOrder::Ascending)?;
        let mut values = Vec::new();
        for index in scan_indexes(&ids, start, |id| id.get()) {
            if values.len() > limit {
                break;
            }
            let value = inner
                .state
                .conversations
                .get(&ids[index])
                .expect("indexed conversation");
            if let Some(owner) = query.owner_conversation_id
                && value.owner.as_ref().map(|edge| edge.conversation_id) != Some(owner)
            {
                continue;
            }
            values.push(value.clone());
        }
        Ok(page_values(
            values,
            limit,
            start.order,
            |record: &ConversationRecord| record.id.get(),
        ))
    }

    async fn entry(
        &self,
        id: EntryId,
        _context: &Context,
    ) -> StorageResult<Option<crate::types::StoredEntry>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        match inner.state.entries.get(&id) {
            None => Ok(None),
            Some(entry) => Ok(Some(crate::types::StoredEntry {
                entry: entry.clone(),
                commit_seq: *inner
                    .state
                    .entry_commit_seqs
                    .get(&id)
                    .expect("entry has commit seq"),
            })),
        }
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        _context: &Context,
    ) -> StorageResult<Option<crate::types::StoredEntry>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        let entries = visible_entries(
            &inner.state,
            conversation_id,
            Some(id.get()),
            Some(id.get()),
        )?;
        match entries.first() {
            None => Ok(None),
            Some(entry) => Ok(Some(crate::types::StoredEntry {
                entry: entry.clone(),
                commit_seq: *inner
                    .state
                    .entry_commit_seqs
                    .get(&id)
                    .expect("entry has commit seq"),
            })),
        }
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        _context: &Context,
    ) -> StorageResult<Option<EntryRecord>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        if !inner.state.conversations.contains_key(&conversation_id) {
            return Err(StorageError::UnknownConversation(conversation_id));
        }
        let mut current_id = conversation_id;
        let mut upper = at_or_before_entry_id.map(EntryId::get).unwrap_or(u64::MAX);
        loop {
            let empty = Vec::new();
            let ids = inner
                .state
                .head_entry_ids
                .get(&current_id)
                .unwrap_or(&empty);
            let index = ids.partition_point(|id| id.get() <= upper);
            if index > 0 {
                let entry = inner
                    .state
                    .entries
                    .get(&ids[index - 1])
                    .expect("indexed entry");
                return Ok(Some(entry.clone()));
            }
            let conversation = inner
                .state
                .conversations
                .get(&current_id)
                .expect("checked above");
            let Some(parent) = &conversation.parent else {
                return Ok(None);
            };
            upper = upper.min(parent.at.get());
            current_id = parent.conversation_id;
        }
    }

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: &Context,
    ) -> StorageResult<Page<EntryRecord>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        let start = scan_start(query.order, cursor.as_ref(), ScanOrder::Descending)?;
        // The cursor narrows the bound on the side the scan moves away from.
        let mut min_entry_id = query.min_entry_id.map(EntryId::get);
        let mut max_entry_id = query.max_entry_id.map(EntryId::get);
        if let Some(after) = start.after {
            if start.order.is_ascending() {
                min_entry_id = Some(min_entry_id.map_or(after + 1, |min| min.max(after + 1)));
            } else {
                max_entry_id = Some(max_entry_id.map_or(after.saturating_sub(1), |max| {
                    max.min(after.saturating_sub(1))
                }));
            }
        }
        let mut entries = if start.order.is_ascending() {
            visible_entries_ascending(
                &inner.state,
                query.conversation_id,
                min_entry_id,
                max_entry_id,
            )?
        } else {
            visible_entries(
                &inner.state,
                query.conversation_id,
                min_entry_id,
                max_entry_id,
            )?
        };
        let next = if entries.len() > limit {
            let last = entries[limit - 1].id;
            entries.truncate(limit);
            Some(Cursor::after_id(last.get(), start.order))
        } else {
            None
        };
        Ok(Page {
            items: entries,
            next,
        })
    }

    async fn task(&self, id: TaskId, _context: &Context) -> StorageResult<Option<StoredTask>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        Ok(inner.state.tasks.get(&id).cloned())
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: &Context,
    ) -> StorageResult<Page<StoredTask>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        let start = scan_start(query.order, cursor.as_ref(), ScanOrder::Ascending)?;
        let ids: Vec<TaskId> = match query.status {
            None => inner.state.task_ids.clone(),
            Some(status) => inner
                .state
                .task_ids_by_status
                .get(&status)
                .cloned()
                .unwrap_or_default(),
        };
        let mut values = Vec::new();
        for index in scan_indexes(&ids, start, |id| id.get()) {
            if values.len() > limit {
                break;
            }
            let value = inner.state.tasks.get(&ids[index]).expect("indexed task");
            if let Some(conversation) = query.conversation_id
                && value.conversation_id != conversation
            {
                continue;
            }
            if let Some(kind) = &query.kind
                && &value.kind != kind
            {
                continue;
            }
            if let Some(abort_requested) = query.abort_requested
                && value.abort_requested != abort_requested
            {
                continue;
            }
            if let Some(background) = query.background
                && value.background != background
            {
                continue;
            }
            values.push(value.clone());
        }
        Ok(page_values(
            values,
            limit,
            start.order,
            |task: &StoredTask| task.id.get(),
        ))
    }

    async fn submission(
        &self,
        id: SubmissionId,
        _context: &Context,
    ) -> StorageResult<Option<SubmissionRecord>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        Ok(inner.state.submissions.get(&id).cloned())
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: &Context,
    ) -> StorageResult<Page<SubmissionRecord>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        let start = scan_start(query.order, cursor.as_ref(), ScanOrder::Ascending)?;
        let ids: Vec<SubmissionId> = match query.status {
            None => inner.state.submission_ids.clone(),
            Some(status) => inner
                .state
                .submission_ids_by_status
                .get(&status)
                .cloned()
                .unwrap_or_default(),
        };
        let mut values = Vec::new();
        for index in scan_indexes(&ids, start, |id| id.get()) {
            if values.len() > limit {
                break;
            }
            let value = inner
                .state
                .submissions
                .get(&ids[index])
                .expect("indexed submission");
            if let Some(conversation) = query.conversation_id
                && value.conversation_id() != conversation
            {
                continue;
            }
            values.push(value.clone());
        }
        Ok(page_values(
            values,
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
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        let Some(id) = inner
            .state
            .submission_ids_by_request
            .get(&conversation_id)
            .and_then(|requests| requests.get(request_id))
        else {
            return Ok(None);
        };
        Ok(inner.state.submissions.get(id).cloned())
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        _context: &Context,
    ) -> StorageResult<Option<DocumentRecord>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        let index = inner.state.document_addresses.get(&address_key(&address));
        if at.is_current() {
            return Ok(index.and_then(|index| index.current_id).map(|id| {
                inner
                    .state
                    .documents
                    .get(&id)
                    .expect("indexed document")
                    .record
                    .clone()
            }));
        }
        for id in index.map(|index| index.ids.as_slice()).unwrap_or(&[]) {
            let record = &inner
                .state
                .documents
                .get(id)
                .expect("indexed document")
                .record;
            if is_alive_at(record, at) {
                return Ok(Some(record.clone()));
            }
        }
        Ok(None)
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        _context: &Context,
    ) -> StorageResult<Option<StoredDocument>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        materialize_document(&inner.state, id, at)
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: &Context,
    ) -> StorageResult<Page<DocumentRecord>> {
        let inner = self.lock()?;
        MemoryStorage::assert_open(&inner)?;
        crate::storage::validate_cursor(cursor.as_ref())?;
        let empty = Vec::new();
        let ids = inner
            .state
            .document_ids_by_scope
            .get(&scope_key(&query.scope))
            .unwrap_or(&empty);
        let start = match cursor.as_ref() {
            None => 0,
            Some(cursor) => ids.partition_point(|id| id.get() <= cursor.after),
        };
        let mut values = Vec::new();
        let mut index = start;
        while index < ids.len() && values.len() <= limit {
            let record = &inner
                .state
                .documents
                .get(&ids[index])
                .expect("indexed document")
                .record;
            index += 1;
            if let Some(kind) = &query.kind
                && &record.kind != kind
            {
                continue;
            }
            if is_alive_at(record, query.at) {
                values.push(record.clone());
            }
        }
        Ok(page_values(
            values,
            limit,
            ScanOrder::Ascending,
            |record: &DocumentRecord| record.id.get(),
        ))
    }

    async fn close(&self, _context: &Context) -> StorageResult<()> {
        let mut inner = self.lock()?;
        inner.closed = true;
        Ok(())
    }
}

/// Convenience constructor matching the upstream `new MemoryStorage()`.
pub fn memory_storage() -> MemoryStorage {
    MemoryStorage::new()
}
