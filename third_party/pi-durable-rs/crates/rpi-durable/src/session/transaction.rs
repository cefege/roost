//! Transaction staging (port of `packages/durable/src/session/transaction.ts`
//! @ `abe508e1b`).
//!
//! A [`Transaction`] stages table writes and document incarnations while the
//! session mutation line is held. The callback settles once; `settle_success`
//! prepares every open change and assembles one atomic [`StorageWrite`] batch,
//! `settle_failure`/`discard` abort every change, and `adopt` performs the
//! pointer-swap adoption plus publication description after storage succeeds.
//!
//! Read-after-write (`types.ts` `ReadAfterWrite`): the first table write sets a
//! flag and later caller table reads reject with
//! [`StorageError::ReadAfterWrite`]. Document access and candidate-task reads
//! remain legal after a table write.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::chord::context::Context;
use crate::chord::json::copy_json;
use crate::chord::tracker::{Change, DocCell, Prepared, Tracker, track};
use crate::documents::{
    DocTarget, DocToken, address_id, check_record_scope_record, check_record_version_record,
    document_create, resolve_address,
};
use crate::errors::StorageError;
use crate::ids::{
    ConversationId, DocumentId, EntryId, ROOT_CONVERSATION_ID, Seq, SubmissionId, TaskId,
    id_from_number,
};
use crate::session::session::SessionImpl;
use crate::session::{
    CommitChange, DocOwner, DocumentChange, DocumentCopyChange, SessionError, SessionResult,
};
use crate::types::{
    ContextEdit, ConversationParent, ConversationQuery, ConversationRecord, Cursor,
    DocumentAddress, DocumentCopySource, DocumentCreate, DocumentPoint, DocumentQuery,
    DocumentRecord, DocumentScope, EntryQuery, EntryRecord, Page, StorageWrite, StoredEntry,
    SubmissionRecord, TaskQuery, TaskRecord, TaskState,
};

/// One eligible existing document change for deferred persistence.
#[derive(Clone)]
pub struct HoldableDocument {
    /// Cached incarnation pinned until the change is stored.
    pub loaded: Arc<LoadedDocument>,
    /// Operations to persist, in order.
    pub ops: Vec<crate::chord::delta::Op>,
    /// Whether checkpoint policy requires storing a base.
    pub base: bool,
    /// Candidate committed value.
    pub value: crate::chord::json::JsonObject,
    /// Published definition version.
    pub version: u64,
    /// Owning conversation.
    pub conversation_id: Option<ConversationId>,
}

const INTERNAL_SCAN_PAGE_SIZE: usize = 256;

/// Defaults a commit binds to: `create_task` conversation and the task attributed
/// to appended entries (upstream `TransactionScope`).
#[derive(Debug, Clone, Copy, Default)]
pub struct TransactionScope {
    /// Default conversation of a conversation-owned task.
    pub conversation_id: Option<ConversationId>,
    /// Task whose runtime commit this is; stamped as `by_task_id` on entries.
    pub task_id: Option<TaskId>,
}

/// Ownership argument of `create_conversation`/`fork_conversation`.
#[derive(Debug, Clone, Copy)]
pub enum ConversationOwnership {
    /// Not owned by a task.
    Ownerless,
    /// Owned by one task.
    Task(TaskId),
}

/// Ownership argument of [`Transaction::create_task`].
#[derive(Debug, Clone, Copy)]
pub enum TaskOwnership {
    /// Owned by the conversation.
    Conversation(ConversationId),
    /// Owned by a parent task.
    Task(TaskId),
}

/// Head marker of an appended entry (upstream `head?: EntryId | "self"`).
#[derive(Debug, Clone, Copy)]
pub enum EntryHead {
    /// The new entry's own ID.
    This,
    /// One explicit entry.
    At(EntryId),
}

/// Fields supplied when appending an immutable entry.
#[derive(Debug, Clone, Default)]
pub struct EntryDraft {
    /// Application-defined discriminator.
    pub kind: String,
    /// Messages contributed to model context.
    pub model: Option<Vec<rpi_ai::types::Message>>,
    /// Application payload.
    pub data: Option<JsonValue>,
    /// Head marker.
    pub head: Option<EntryHead>,
    /// Context-only overrides.
    pub edits: Option<Vec<ContextEdit>>,
}

impl EntryDraft {
    /// A draft of the given kind with no other fields set.
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            ..Self::default()
        }
    }
}

/// Raw fields of a submission created by [`Transaction::create_submission`].
#[derive(Debug, Clone)]
pub enum SubmissionCreate {
    /// A user input submission.
    Input {
        /// Owning conversation.
        conversation_id: ConversationId,
        /// Host deduplication key.
        request_id: Option<String>,
    },
    /// A passive write submission.
    Write {
        /// Owning conversation.
        conversation_id: ConversationId,
        /// Host deduplication key.
        request_id: Option<String>,
    },
}

impl SubmissionCreate {
    fn conversation_id(&self) -> ConversationId {
        match self {
            SubmissionCreate::Input {
                conversation_id, ..
            }
            | SubmissionCreate::Write {
                conversation_id, ..
            } => *conversation_id,
        }
    }

    fn into_record(self, id: SubmissionId) -> SubmissionRecord {
        use crate::types::{InputSubmissionStatus, WriteSubmissionStatus};
        match self {
            SubmissionCreate::Input {
                conversation_id,
                request_id,
            } => SubmissionRecord::Input {
                id,
                conversation_id,
                request_id,
                status: InputSubmissionStatus::Queued,
            },
            SubmissionCreate::Write {
                conversation_id,
                request_id,
            } => SubmissionRecord::Write {
                id,
                conversation_id,
                request_id,
                status: WriteSubmissionStatus::Queued,
            },
        }
    }
}

/// One submission settlement (upstream `SubmissionSettlement`).
#[derive(Debug, Clone)]
pub enum SubmissionSettlement {
    /// A placed input answered.
    Done {
        /// Answer entry.
        answer: EntryId,
    },
    /// A terminal input or write that could not be placed.
    Unanswered {
        /// Reason.
        reason: String,
        /// Optional detail.
        detail: Option<JsonValue>,
    },
}

#[derive(Debug, Clone)]
enum SubmissionChange {
    Placed(EntryId),
    Settled(SubmissionSettlement),
}

/// One staged task candidate.
#[derive(Debug, Clone)]
struct TaskWrite {
    kind: TaskWriteKind,
    record: TaskRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskWriteKind {
    Create,
    Replace,
}

#[derive(Debug, Default, Clone)]
struct TransactionTask {
    committed_read: Option<Option<TaskRecord>>,
    write: Option<TaskWrite>,
    publication_conversation_id: Option<ConversationId>,
}

/// Storage/cache provenance of one staged incarnation.
enum DocumentTarget {
    Loaded(Arc<LoadedDocument>),
    Created {
        record: DocumentCreate,
        version: u64,
        tracker: Arc<Tracker>,
    },
    ForkCopy {
        record: DocumentCreate,
        source: DocumentCopySource,
    },
    RetireOnly {
        record: DocumentRecord,
    },
}

/// One document incarnation acquired, created, or retired by this transaction.
struct DocumentEntry {
    address_id: String,
    address: DocumentAddress,
    definition: Option<Arc<crate::documents::DocDefinition>>,
    draft: Option<DocDraft>,
    target: Option<DocumentTarget>,
    change: Option<Change>,
    prepared: Option<Prepared>,
    retire_on_commit: bool,
}

/// One committed document incarnation owned by the session tracker cache.
pub struct LoadedDocument {
    /// Cache address identity.
    pub address_id: String,
    /// Persisted incarnation record.
    pub record: DocumentRecord,
    stored_version: std::sync::atomic::AtomicU64,
    /// Definition version whose shape the tracked value has.
    pub value_version: u64,
    deltas_since_base: std::sync::atomic::AtomicU64,
    /// Committed value tracker.
    pub tracker: Arc<Tracker>,
}

impl LoadedDocument {
    /// Build a loaded incarnation.
    pub fn new(
        address_id: String,
        record: DocumentRecord,
        stored_version: u64,
        value_version: u64,
        deltas_since_base: u64,
        tracker: Arc<Tracker>,
    ) -> Self {
        Self {
            address_id,
            record,
            stored_version: std::sync::atomic::AtomicU64::new(stored_version),
            value_version,
            deltas_since_base: std::sync::atomic::AtomicU64::new(deltas_since_base),
            tracker,
        }
    }

    /// Persisted definition version.
    pub fn stored_version(&self) -> u64 {
        self.stored_version
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Set the persisted definition version after a migration base write.
    pub fn set_stored_version(&self, version: u64) {
        self.stored_version
            .store(version, std::sync::atomic::Ordering::SeqCst);
    }

    /// Count of stored deltas after the newest base.
    pub fn deltas_since_base(&self) -> u64 {
        self.deltas_since_base
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Set the stored delta count.
    pub fn set_deltas_since_base(&self, value: u64) {
        self.deltas_since_base
            .store(value, std::sync::atomic::Ordering::SeqCst);
    }
}

/// One open document draft handed to a commit callback. `read`/`write` are
/// revoked once the transaction settles (upstream `Draft<T>`).
#[derive(Clone)]
pub struct DocDraft {
    /// Shared staging cell.
    pub cell: Arc<DocCell>,
    /// Cache address identity.
    pub address_id: String,
}

impl DocDraft {
    /// Read the current candidate value.
    pub fn read(&self) -> Result<JsonValue, crate::chord::delta::DeltaError> {
        self.cell.read()
    }

    /// Lock the candidate value for in-place mutation.
    pub fn write(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, JsonValue>, crate::chord::delta::DeltaError> {
        self.cell.write()
    }

    /// Cache address identity.
    pub fn address_id(&self) -> &str {
        &self.address_id
    }
}

enum PlanRecord {
    Create(DocumentCreate),
    Committed(DocumentRecord),
}

enum TrackerRef {
    Created(Arc<Tracker>),
    Loaded(Arc<LoadedDocument>),
}

struct PlanChange {
    tracker: TrackerRef,
    prepared: Prepared,
    version: u64,
    loaded: Option<Arc<LoadedDocument>>,
    definition: Option<Arc<crate::documents::DocDefinition>>,
}

struct DocumentPlan {
    address_id: String,
    record: PlanRecord,
    retire: bool,
    content: Option<StorageWrite>,
    change: Option<PlanChange>,
    conversation_id: Option<ConversationId>,
}

impl DocumentPlan {
    fn record_id(&self) -> DocumentId {
        match &self.record {
            PlanRecord::Create(record) => record.id,
            PlanRecord::Committed(record) => record.id,
        }
    }

    fn scope(&self) -> DocumentScope {
        match &self.record {
            PlanRecord::Create(record) => record.scope.clone(),
            PlanRecord::Committed(record) => record.scope.clone(),
        }
    }

    fn fork(&self) -> Option<crate::types::DocumentFork> {
        match &self.record {
            PlanRecord::Create(record) => record.fork,
            PlanRecord::Committed(record) => record.fork,
        }
    }
}

fn publishes(plan: &DocumentPlan) -> bool {
    plan.retire
        || plan
            .change
            .as_ref()
            .map(|c| c.loaded.is_none())
            .unwrap_or(true)
        || plan.content.is_some()
}

/// A transaction for one session commit callback.
pub struct Transaction {
    host: Arc<SessionImpl>,
    context: Context,
    scope: TransactionScope,
    sealed: bool,
    has_table_write: bool,
    writes: Vec<StorageWrite>,
    created_conversation_ids: std::collections::BTreeSet<ConversationId>,
    fork_source_conversation_ids: std::collections::BTreeSet<ConversationId>,
    fork_source_document_ids: std::collections::BTreeSet<DocumentId>,
    tasks: BTreeMap<TaskId, TransactionTask>,
    submissions: BTreeMap<SubmissionId, SubmissionRecord>,
    submission_changes: Vec<(SubmissionId, SubmissionChange)>,
    documents: Vec<DocumentEntry>,
    latest_document_by_address: BTreeMap<String, usize>,
    plans: Vec<DocumentPlan>,
}

impl Transaction {
    pub(crate) fn new(host: Arc<SessionImpl>, context: Context, scope: TransactionScope) -> Self {
        Self {
            host,
            context,
            scope,
            sealed: false,
            has_table_write: false,
            writes: Vec::new(),
            created_conversation_ids: Default::default(),
            fork_source_conversation_ids: Default::default(),
            fork_source_document_ids: Default::default(),
            tasks: Default::default(),
            submissions: Default::default(),
            submission_changes: Default::default(),
            documents: Vec::new(),
            latest_document_by_address: Default::default(),
            plans: Vec::new(),
        }
    }

    // ─── Table reads ────────────────────────────────────────────────────────

    /// Look up one conversation by exact ID.
    pub async fn conversation(
        &self,
        id: ConversationId,
    ) -> SessionResult<Option<ConversationRecord>> {
        self.read("conversation")?;
        Ok(self.host.storage().conversation(id, &self.context).await?)
    }

    /// Look up one global entry and the sequence that persisted it.
    pub async fn entry(&self, id: EntryId) -> SessionResult<Option<EntryRecord>> {
        self.read("entry")?;
        Ok(self
            .host
            .storage()
            .entry(id, &self.context)
            .await?
            .map(|stored| stored.entry))
    }

    /// Look up one entry only when it is visible from the requested conversation.
    pub async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
    ) -> SessionResult<Option<StoredEntry>> {
        self.read("entry")?;
        Ok(self
            .host
            .storage()
            .entry_in_conversation(conversation_id, id, &self.context)
            .await?)
    }

    /// Look up the latest complete record for one task.
    pub async fn task(&mut self, id: TaskId) -> SessionResult<Option<TaskRecord>> {
        self.read("task")?;
        self.committed_task(id).await
    }

    /// Scan conversations.
    pub async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
    ) -> SessionResult<Page<ConversationRecord>> {
        self.read("scanConversations")?;
        Ok(self
            .host
            .storage()
            .scan_conversations(query, limit, cursor, &self.context)
            .await?)
    }

    /// Scan fork-aware entries.
    pub async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
    ) -> SessionResult<Page<EntryRecord>> {
        self.read("scanEntries")?;
        Ok(self
            .host
            .storage()
            .scan_entries(query, limit, cursor, &self.context)
            .await?)
    }

    /// Newest visible head marker of one conversation.
    pub async fn latest_head_marker(
        &self,
        conversation_id: ConversationId,
    ) -> SessionResult<Option<EntryRecord>> {
        self.read("latestHeadMarker")?;
        Ok(self
            .host
            .storage()
            .find_latest_head_marker(conversation_id, None, &self.context)
            .await?)
    }

    /// Scan tasks.
    pub async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
    ) -> SessionResult<Page<TaskRecord>> {
        self.read("scanTasks")?;
        Ok(self
            .host
            .storage()
            .scan_tasks(query, limit, cursor, &self.context)
            .await?)
    }

    /// Committed submission record.
    pub async fn submission(&self, id: SubmissionId) -> SessionResult<Option<SubmissionRecord>> {
        self.read("submission")?;
        Ok(self.host.storage().submission(id, &self.context).await?)
    }

    /// Committed submission with a conversation-scoped request ID.
    pub async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
    ) -> SessionResult<Option<SubmissionRecord>> {
        self.read("submissionByRequest")?;
        Ok(self
            .host
            .storage()
            .submission_by_request(conversation_id, request_id, &self.context)
            .await?)
    }

    // ─── Table writes ───────────────────────────────────────────────────────

    /// Create one conversation (upstream `createConversation`).
    pub async fn create_conversation(
        &mut self,
        ownership: ConversationOwnership,
    ) -> SessionResult<ConversationRecord> {
        self.assert_writable()?;
        self.has_table_write = true;
        self.stage_conversation(None, ownership, None).await
    }

    /// Create the reserved root conversation (upstream internal bootstrap path).
    pub async fn create_root_conversation(&mut self) -> SessionResult<ConversationRecord> {
        self.assert_writable()?;
        self.has_table_write = true;
        self.stage_conversation(
            None,
            ConversationOwnership::Ownerless,
            Some(ROOT_CONVERSATION_ID),
        )
        .await
    }

    /// Fork one conversation at `at`.
    pub async fn fork_conversation(
        &mut self,
        parent_conversation_id: ConversationId,
        at: EntryId,
        ownership: ConversationOwnership,
    ) -> SessionResult<ConversationRecord> {
        self.assert_writable()?;
        self.has_table_write = true;
        self.stage_conversation(
            Some(ConversationParent {
                conversation_id: parent_conversation_id,
                at,
            }),
            ownership,
            None,
        )
        .await
    }

    async fn stage_conversation(
        &mut self,
        parent: Option<ConversationParent>,
        ownership: ConversationOwnership,
        reserved: Option<ConversationId>,
    ) -> SessionResult<ConversationRecord> {
        let owner_task_id = match ownership {
            ConversationOwnership::Task(task_id) => Some(task_id),
            ConversationOwnership::Ownerless => None,
        };
        let id = match reserved {
            Some(id) => id,
            None => {
                id_from_number::<crate::ids::Conversation>(self.host.storage().mint_id().await?)
            }
        };
        self.assert_open()?;
        let mut owner = None;
        if let Some(task_id) = owner_task_id {
            let task = self.current_task(task_id).await?;
            self.assert_open()?;
            let Some(task) = task else {
                return Err(SessionError::message(format!(
                    "Conversation owner task {task_id} does not exist"
                )));
            };
            owner = Some(crate::types::ConversationOwner {
                conversation_id: task.conversation_id,
                task_id,
            });
        }
        let record = ConversationRecord {
            id,
            parent: parent.clone(),
            owner,
        };
        if let Some(parent) = &parent {
            let copies = crate::session::forks::prepare_fork_document_copies(
                self.host.storage().as_ref(),
                self.host.storage().as_ref(),
                parent.conversation_id,
                parent.at,
                id,
                &self.context,
            )
            .await?;
            self.assert_open()?;
            for copy in copies {
                self.fork_source_document_ids.insert(copy.source.id);
                let address = DocumentAddress {
                    kind: copy.record.kind.clone(),
                    scope: copy.record.scope.clone(),
                    key: copy.record.key.clone(),
                };
                let entry = DocumentEntry {
                    address_id: address_id(&address),
                    address,
                    definition: None,
                    draft: None,
                    target: Some(DocumentTarget::ForkCopy {
                        record: copy.record,
                        source: copy.source,
                    }),
                    change: None,
                    prepared: None,
                    retire_on_commit: false,
                };
                self.push_document(entry);
            }
            self.fork_source_conversation_ids
                .insert(parent.conversation_id);
        }
        self.created_conversation_ids.insert(id);
        self.writes.push(StorageWrite::Conversation {
            value: record.clone(),
        });
        let host = Arc::clone(&self.host);
        host.conversation_created(self, &record).await?;
        self.assert_open()?;
        Ok(record)
    }

    /// Append one immutable entry.
    pub async fn append_entry(
        &mut self,
        conversation_id: ConversationId,
        draft: EntryDraft,
    ) -> SessionResult<EntryRecord> {
        self.assert_writable()?;
        self.has_table_write = true;
        self.require_conversation(conversation_id).await?;
        self.assert_open()?;
        let id = id_from_number::<crate::ids::Entry>(self.host.storage().mint_id().await?);
        self.assert_open()?;
        let head = match draft.head {
            None => None,
            Some(EntryHead::This) => Some(id),
            Some(EntryHead::At(entry)) => Some(entry),
        };
        let record = EntryRecord {
            id,
            conversation_id,
            kind: draft.kind,
            model: draft.model,
            data: draft.data,
            head,
            edits: draft.edits,
            by_task_id: self.scope.task_id,
        };
        self.writes.push(StorageWrite::Entry {
            value: record.clone(),
        });
        Ok(record)
    }

    /// Create one task record from its definition's initial checkpoint.
    pub async fn create_task(&mut self, create: TaskCreate) -> SessionResult<TaskId> {
        self.assert_writable()?;
        self.has_table_write = true;
        let mut owner_record: Option<TaskRecord> = None;
        if let TaskOwnership::Task(owner_id) = create.ownership {
            let owner = self.current_task(owner_id).await?;
            self.assert_open()?;
            let Some(owner) = owner else {
                return Err(SessionError::message(format!(
                    "Task owner {owner_id} does not exist"
                )));
            };
            if create.background {
                return Err(SessionError::message("A child task cannot be background"));
            }
            if let Some(conversation_id) = create.conversation_id
                && conversation_id != owner.conversation_id
            {
                return Err(SessionError::message(format!(
                    "A child task lives in its owner's conversation {}",
                    owner.conversation_id
                )));
            }
            owner_record = Some(owner);
        }
        let explicit_conversation = match create.ownership {
            TaskOwnership::Conversation(id) => Some(id),
            TaskOwnership::Task(_) => create.conversation_id,
        };
        let conversation_id = owner_record
            .as_ref()
            .map(|owner| owner.conversation_id)
            .or(explicit_conversation)
            .or(self.scope.conversation_id)
            .ok_or_else(|| {
                SessionError::message("Tx.create_task() requires options.conversationId")
            })?;
        self.require_conversation(conversation_id).await?;
        self.assert_open()?;
        let id = id_from_number::<crate::ids::Task>(self.host.storage().mint_id().await?);
        self.assert_open()?;
        let record = TaskRecord {
            id,
            conversation_id,
            kind: create.kind,
            version: create.version,
            input: create.input,
            owner: owner_record.as_ref().map(|owner| owner.id),
            background: create.background,
            abort_requested: false,
            started_at: None,
            ended_at: None,
            state: TaskState::Pending {
                checkpoint: create.checkpoint,
            },
            memos: None,
        };
        self.tasks.entry(id).or_default().write = Some(TaskWrite {
            kind: TaskWriteKind::Create,
            record,
        });
        Ok(id)
    }

    /// Create a raw submission record with a fresh ID.
    pub async fn create_submission(
        &mut self,
        create: SubmissionCreate,
    ) -> SessionResult<SubmissionRecord> {
        self.assert_writable()?;
        self.has_table_write = true;
        let conversation_id = create.conversation_id();
        self.require_conversation(conversation_id).await?;
        self.assert_open()?;
        let id = id_from_number::<crate::ids::Submission>(self.host.storage().mint_id().await?);
        self.assert_open()?;
        let record = create.into_record(id);
        self.submissions.insert(id, record.clone());
        Ok(record)
    }

    /// Stage one submission settlement (resolved during assembly).
    pub fn settle_submission(
        &mut self,
        id: SubmissionId,
        settlement: SubmissionSettlement,
    ) -> SessionResult<()> {
        self.assert_writable()?;
        self.has_table_write = true;
        self.submission_changes
            .push((id, SubmissionChange::Settled(settlement)));
        Ok(())
    }

    /// Stage the placement of a queued submission at `entry`.
    pub fn place_submission(&mut self, id: SubmissionId, entry: EntryId) -> SessionResult<()> {
        self.assert_writable()?;
        self.has_table_write = true;
        self.submission_changes
            .push((id, SubmissionChange::Placed(entry)));
        Ok(())
    }

    /// Replace one task record completely (internal task replacement path).
    pub fn set_task(&mut self, value: TaskRecord) -> SessionResult<()> {
        self.assert_writable()?;
        self.has_table_write = true;
        let existing = self.tasks.get(&value.id).cloned().unwrap_or_default();
        if let Some(write) = &existing.write {
            if is_terminal(&write.record.state) {
                return Err(SessionError::message(format!(
                    "Task {} already has a terminal candidate",
                    value.id
                )));
            }
            if write.record.conversation_id != value.conversation_id {
                return Err(SessionError::message(format!(
                    "Task {} cannot change conversations",
                    value.id
                )));
            }
        }
        let stamped = self.stamp_times(value, existing.write.as_ref().map(|w| &w.record));
        let kind = if existing.write.as_ref().map(|w| w.kind) == Some(TaskWriteKind::Create) {
            TaskWriteKind::Create
        } else {
            TaskWriteKind::Replace
        };
        let entry = self.tasks.entry(stamped.id).or_default();
        entry.write = Some(TaskWrite {
            kind,
            record: stamped,
        });
        Ok(())
    }

    /// Candidate records of the tasks this transaction created or replaced so far.
    pub fn staged_tasks(&self) -> Vec<TaskRecord> {
        self.tasks
            .values()
            .filter_map(|task| task.write.as_ref().map(|write| write.record.clone()))
            .collect()
    }

    /// Conversations this transaction created or forked so far.
    pub fn staged_conversations(&self) -> Vec<ConversationRecord> {
        self.writes
            .iter()
            .filter_map(|write| match write {
                StorageWrite::Conversation { value } => Some(value.clone()),
                _ => None,
            })
            .collect()
    }

    // ─── Documents ──────────────────────────────────────────────────────────

    /// Acquire one document incarnation draft, creating it when absent.
    ///
    /// `seed` is the family seed passed to `definition.initial`; it is ignored
    /// for singleton definitions (upstream `initial(seed)`).
    pub async fn doc(
        &mut self,
        token: &DocToken,
        owner: DocOwner<'_>,
        seed: Option<JsonValue>,
    ) -> SessionResult<DocDraft> {
        self.assert_open()?;
        let definition = Arc::clone(&token.definition);
        let resolved = resolve_address(&definition, doc_target(&definition, owner))?;
        self.assert_task_documents_open(&resolved.address)?;
        let skip_load = self
            .latest_index(&resolved.id)
            .map(|index| self.documents[index].retire_on_commit)
            .unwrap_or(false);
        if let Some(index) = self.latest_index(&resolved.id)
            && !self.documents[index].retire_on_commit
        {
            if let Some(draft) = &self.documents[index].draft {
                return Ok(draft.clone());
            }
            if matches!(
                self.documents[index].target,
                Some(DocumentTarget::ForkCopy { .. })
            ) {
                return self.acquire_fork_copy(index, Arc::clone(&definition)).await;
            }
        }
        let seed = if definition.family {
            Some(copy_json(seed.as_ref().unwrap_or(&JsonValue::Null)))
        } else {
            None
        };
        let entry = DocumentEntry {
            address_id: resolved.id.clone(),
            address: resolved.address.clone(),
            definition: Some(definition),
            draft: None,
            target: None,
            change: None,
            prepared: None,
            retire_on_commit: false,
        };
        let index = self.push_document(entry);
        self.acquire(index, skip_load, seed).await
    }

    async fn acquire(
        &mut self,
        index: usize,
        skip_load: bool,
        seed: Option<JsonValue>,
    ) -> SessionResult<DocDraft> {
        let definition = self.documents[index]
            .definition
            .clone()
            .ok_or_else(|| SessionError::message("Document definition is missing"))?;
        let address = self.documents[index].address.clone();
        let address_id = self.documents[index].address_id.clone();
        let loaded = if skip_load {
            None
        } else {
            self.host
                .load_document(&definition, &address_id, &address, &self.context)
                .await?
        };
        self.assert_open()?;
        if let Some(loaded) = loaded {
            check_record_scope_record(&definition, &loaded.record)?;
            check_record_version_record(&definition, &loaded.record, loaded.stored_version())?;
            let change = loaded.tracker.begin_change();
            let draft = DocDraft {
                cell: change.state(),
                address_id,
            };
            self.documents[index].target = Some(DocumentTarget::Loaded(loaded));
            self.documents[index].change = Some(change);
            self.documents[index].draft = Some(draft.clone());
            return Ok(draft);
        }
        match &address.scope {
            DocumentScope::Conversation { conversation_id } => {
                self.require_conversation(*conversation_id).await?;
            }
            DocumentScope::Task { task_id } => {
                let task = self.current_task(*task_id).await?;
                let Some(task) = task else {
                    return Err(SessionError::message(format!(
                        "Task {task_id} does not exist"
                    )));
                };
                if is_terminal(&task.state) {
                    return Err(SessionError::message(format!("Task {task_id} is terminal")));
                }
            }
            DocumentScope::Session => {}
        }
        self.assert_writable()?;
        let value = (definition.initial)(seed.as_ref());
        let id = id_from_number::<crate::ids::Document>(self.host.storage().mint_id().await?);
        self.assert_open()?;
        let tracker = Arc::new(track(JsonValue::Object(value)));
        let change = tracker.begin_change();
        let record = document_create(&definition, &address, id);
        let draft = DocDraft {
            cell: change.state(),
            address_id,
        };
        self.documents[index].target = Some(DocumentTarget::Created {
            record,
            version: definition.version,
            tracker,
        });
        self.documents[index].change = Some(change);
        self.documents[index].draft = Some(draft.clone());
        Ok(draft)
    }

    async fn acquire_fork_copy(
        &mut self,
        index: usize,
        definition: Arc<crate::documents::DocDefinition>,
    ) -> SessionResult<DocDraft> {
        let (record, source) = match &self.documents[index].target {
            Some(DocumentTarget::ForkCopy { record, source }) => (record.clone(), source.clone()),
            _ => return Err(SessionError::message("Document is not a fork copy")),
        };
        let stored = self
            .host
            .storage()
            .document(source.id, source.at, &self.context)
            .await?
            .ok_or_else(|| {
                SessionError::message(format!("Fork source document {} cannot be read", source.id))
            })?;
        self.assert_open()?;
        if !matches!(stored.record.scope, DocumentScope::Conversation { .. })
            || stored.record.kind != record.kind
            || stored.record.key != record.key
            || stored.record.history != record.history
            || stored.record.fork != record.fork
        {
            return Err(SessionError::message(format!(
                "Fork source document {} does not match the copied record",
                source.id
            )));
        }
        let value = crate::documents::materialize_document_value(
            &definition,
            &stored.record,
            stored.version,
            &stored.value,
        )?;
        let tracker = Arc::new(track(JsonValue::Object(value)));
        let change = tracker.begin_change();
        let draft = DocDraft {
            cell: change.state(),
            address_id: self.documents[index].address_id.clone(),
        };
        self.documents[index].target = Some(DocumentTarget::Created {
            record,
            version: definition.version,
            tracker,
        });
        self.documents[index].change = Some(change);
        self.documents[index].draft = Some(draft.clone());
        Ok(draft)
    }

    /// Stage the retirement of the incarnation currently occupying an address.
    pub async fn retire_doc(&mut self, token: &DocToken, owner: DocOwner<'_>) -> SessionResult<()> {
        self.assert_writable()?;
        let definition = Arc::clone(&token.definition);
        let resolved = resolve_address(&definition, doc_target(&definition, owner))?;
        if let Some(index) = self.latest_index(&resolved.id) {
            if self.documents[index].retire_on_commit {
                return Ok(());
            }
            if let Some(DocumentTarget::ForkCopy { record, .. }) = &self.documents[index].target {
                crate::documents::check_record_scope(&definition, record)?;
                self.documents[index].retire_on_commit = true;
                return Ok(());
            }
            if self.documents[index].draft.is_some() {
                self.documents[index].retire_on_commit = true;
                return Ok(());
            }
        }
        let entry = DocumentEntry {
            address_id: resolved.id.clone(),
            address: resolved.address.clone(),
            definition: Some(definition),
            draft: None,
            target: None,
            change: None,
            prepared: None,
            retire_on_commit: true,
        };
        let index = self.push_document(entry);
        self.find_retirement(index).await
    }

    async fn find_retirement(&mut self, index: usize) -> SessionResult<()> {
        let address_id = self.documents[index].address_id.clone();
        let address = self.documents[index].address.clone();
        let cached = self.host.cached(&address_id);
        let record = match cached {
            Some(loaded) => Some(loaded.record.clone()),
            None => {
                self.host
                    .storage()
                    .find_document(address, DocumentPoint::CURRENT, &self.context)
                    .await?
            }
        };
        self.assert_open()?;
        if let Some(record) = record {
            let definition = self.documents[index]
                .definition
                .clone()
                .ok_or_else(|| SessionError::message("Document definition is missing"))?;
            check_record_scope_record(&definition, &record)?;
            self.documents[index].target = Some(DocumentTarget::RetireOnly { record });
        }
        Ok(())
    }

    // ─── Settlement ─────────────────────────────────────────────────────────

    /// Seal after callback failure: abort every change.
    pub async fn settle_failure(&mut self) {
        self.sealed = true;
        self.abort_changes();
    }

    /// Seal after callback success, prepare every open change, and assemble the
    /// atomic batch. Any failure aborts every change before storage admission.
    pub async fn settle_success(&mut self) -> SessionResult<Vec<StorageWrite>> {
        self.sealed = true;
        let result = self.prepare_and_assemble().await;
        match result {
            Ok(writes) => Ok(writes),
            Err(error) => {
                self.abort_changes();
                Err(error)
            }
        }
    }

    async fn prepare_and_assemble(&mut self) -> SessionResult<Vec<StorageWrite>> {
        for document in &mut self.documents {
            if let Some(change) = &mut document.change {
                document.prepared = Some(change.prepare()?);
            }
        }
        self.assemble().await
    }

    /// Abort every open change (upstream `discard`).
    pub fn discard(&mut self) {
        self.abort_changes();
    }
    /// Return changes that can be adopted in memory without writing tables or creating document incarnations.
    pub fn holdable(&self) -> Option<Vec<HoldableDocument>> {
        if self
            .writes
            .iter()
            .any(|write| !matches!(write, StorageWrite::DocumentChange { .. }))
        {
            return None;
        }
        if self.tasks.values().any(|task| task.write.is_some())
            || !self.submissions.is_empty()
            || !self.submission_changes.is_empty()
        {
            return None;
        }

        let mut held = Vec::new();
        for plan in &self.plans {
            if plan.content.is_none() {
                continue;
            }
            if plan.retire {
                return None;
            }
            let change = plan.change.as_ref()?;
            let loaded = change.loaded.as_ref()?;
            let record = &loaded.record;
            if !matches!(
                (&record.scope, record.history, record.fork),
                (
                    DocumentScope::Conversation { .. },
                    Some(crate::types::DocumentHistory::Latest),
                    Some(crate::types::DocumentFork::Initial)
                )
            ) || loaded.stored_version() != change.version
            {
                return None;
            }
            let StorageWrite::DocumentChange { content, .. } = plan.content.as_ref()? else {
                return None;
            };
            let value = change
                .prepared
                .value()
                .as_object()
                .cloned()
                .unwrap_or_default();
            held.push(HoldableDocument {
                loaded: Arc::clone(loaded),
                ops: change.prepared.ops().to_vec(),
                base: content.is_base(),
                value,
                version: change.version,
                conversation_id: plan.conversation_id,
            });
        }
        (!held.is_empty()).then_some(held)
    }

    /// Adopt eligible prepared changes in memory without advancing persisted revision metadata.
    pub fn adopt_held(&self) -> SessionResult<Vec<CommitChange>> {
        let mut publications = Vec::new();
        for plan in &self.plans {
            let Some(change) = &plan.change else {
                continue;
            };
            let Some(loaded) = &change.loaded else {
                continue;
            };
            if change.prepared.ops().is_empty() {
                change.prepared.abort();
                continue;
            }
            match &change.tracker {
                TrackerRef::Created(_) => {
                    return Err(SessionError::message(
                        "Deferred document change unexpectedly has a new tracker",
                    ));
                }
                TrackerRef::Loaded(document) => {
                    document.tracker.adopt(change.prepared.clone())?;
                }
            }
            publications.push(CommitChange::Document(DocumentChange {
                record: loaded.record.clone(),
                conversation_id: plan.conversation_id,
                version: Some(change.version),
                value: change.prepared.value().as_object().cloned(),
                ops: change.prepared.ops().to_vec(),
            }));
        }
        Ok(publications)
    }

    /// Adopt every prepared change by pointer swap and describe the publication.
    pub fn adopt(&self, seq: Seq) -> SessionResult<Vec<CommitChange>> {
        let mut publications = Vec::new();
        for plan in &self.plans {
            let committed = matches!(plan.record, PlanRecord::Committed(_));
            let mut record: DocumentRecord = match &plan.record {
                PlanRecord::Committed(record) => record.clone(),
                PlanRecord::Create(create) => DocumentRecord {
                    id: create.id,
                    kind: create.kind.clone(),
                    key: create.key.clone(),
                    created_at: seq,
                    retired_at: None,
                    scope: create.scope.clone(),
                    history: create.history,
                    fork: create.fork,
                },
            };
            if plan.retire {
                record.retired_at = Some(seq);
            }
            if let Some(change) = &plan.change {
                let adopt = if change.loaded.is_none() {
                    !plan.retire
                } else {
                    !change.prepared.ops().is_empty()
                };
                if adopt {
                    match &change.tracker {
                        TrackerRef::Created(tracker) => tracker.adopt(change.prepared.clone())?,
                        TrackerRef::Loaded(loaded) => {
                            loaded.tracker.adopt(change.prepared.clone())?
                        }
                    }
                } else {
                    change.prepared.abort();
                }
                if let Some(loaded) = &change.loaded {
                    if loaded.stored_version() < change.version {
                        loaded.set_stored_version(change.version);
                    }
                    if let Some(StorageWrite::DocumentChange { content, .. }) = &plan.content {
                        if content.is_base() {
                            loaded.set_deltas_since_base(0);
                        } else {
                            loaded.set_deltas_since_base(loaded.deltas_since_base() + 1);
                        }
                    }
                } else if !plan.retire {
                    let tracker = match &change.tracker {
                        TrackerRef::Created(tracker) => Arc::clone(tracker),
                        TrackerRef::Loaded(loaded) => Arc::clone(&loaded.tracker),
                    };
                    self.host.install(Arc::new(LoadedDocument::new(
                        plan.address_id.clone(),
                        record.clone(),
                        change.version,
                        change.version,
                        0,
                        tracker,
                    )));
                }
            }
            if plan.retire {
                if committed {
                    self.host.evict(&plan.address_id, record.id);
                }
                publications.push(CommitChange::Document(DocumentChange {
                    record,
                    conversation_id: plan.conversation_id,
                    version: None,
                    value: None,
                    ops: Vec::new(),
                }));
            } else if let Some(StorageWrite::DocumentCopy { source, .. }) = &plan.content {
                publications.push(CommitChange::DocumentCopy(DocumentCopyChange {
                    record,
                    conversation_id: plan.conversation_id.unwrap_or(ROOT_CONVERSATION_ID),
                    source: source.clone(),
                }));
            } else if let Some(change) = &plan.change
                && publishes(plan)
            {
                let ops = if change.loaded.is_none() {
                    Vec::new()
                } else {
                    change.prepared.ops().to_vec()
                };
                let value = change.prepared.value().as_object().cloned();
                publications.push(CommitChange::Document(DocumentChange {
                    record,
                    conversation_id: plan.conversation_id,
                    version: Some(change.version),
                    value,
                    ops,
                }));
            }
        }
        Ok(publications)
    }

    // ─── Assembly ───────────────────────────────────────────────────────────

    async fn assemble(&mut self) -> SessionResult<Vec<StorageWrite>> {
        let mut plans = Vec::new();
        for document in &self.documents {
            if let Some(plan) = plan_document(document)? {
                plans.push(plan);
            }
        }
        reject_fork_source_writes(
            &plans,
            &self.fork_source_document_ids,
            &self.fork_source_conversation_ids,
        )?;
        self.validate_owners().await?;
        self.validate_task_replacements().await?;
        self.retire_terminal_task_documents(&mut plans).await?;
        self.resolve_publication_owners(&mut plans).await?;
        self.resolve_submission_changes().await?;

        let mut writes = self.writes.clone();
        for value in self.submissions.values() {
            writes.push(StorageWrite::Submission {
                value: value.clone(),
            });
        }
        for task in self.tasks.values() {
            if let Some(write) = &task.write {
                writes.push(StorageWrite::Task {
                    value: write.record.clone(),
                });
            }
        }
        for plan in &mut plans {
            if let Some(StorageWrite::DocumentChange { content, .. }) = &plan.content {
                let is_delta = !content.is_base();
                if is_delta
                    && let Some(change) = &plan.change
                    && let Some(loaded) = &change.loaded
                    && let Some(definition) = &change.definition
                    && let Some(predicate) = &definition.checkpoint_when
                {
                    let deltas_since_base = loaded.deltas_since_base();
                    if predicate(
                        change.prepared.value(),
                        change.prepared.ops(),
                        deltas_since_base,
                    ) {
                        // Replace the delta with a full base of the prepared value.
                        if let Some(StorageWrite::DocumentChange { id, .. }) = plan.content.take() {
                            plan.content = Some(StorageWrite::DocumentChange {
                                id,
                                content: crate::types::DocumentContent::Base {
                                    version: change.version,
                                    value: change
                                        .prepared
                                        .value()
                                        .as_object()
                                        .cloned()
                                        .unwrap_or_default(),
                                },
                            });
                        }
                    }
                }
            }
            if let Some(content) = &plan.content {
                writes.push(content.clone());
            }
            if plan.retire {
                writes.push(StorageWrite::DocumentRetire {
                    id: plan.record_id(),
                });
            }
        }
        self.plans = plans;
        self.writes = writes.clone();
        Ok(writes)
    }

    async fn validate_owners(&mut self) -> SessionResult<()> {
        let mut owners: Vec<(&'static str, TaskId)> = Vec::new();
        for write in &self.writes {
            if let StorageWrite::Conversation { value } = write
                && let Some(owner) = &value.owner
            {
                owners.push(("Conversation owner task", owner.task_id));
            }
        }
        for task in self.tasks.values() {
            if let Some(write) = &task.write
                && write.kind == TaskWriteKind::Create
                && let Some(owner) = write.record.owner
            {
                owners.push(("Task owner", owner));
            }
        }
        for (what, task_id) in owners {
            let task = self.current_task(task_id).await?;
            let Some(task) = task else {
                return Err(SessionError::message(format!(
                    "{what} {task_id} does not exist"
                )));
            };
            if is_terminal(&task.state) || is_completing(&task.state) {
                return Err(SessionError::message(format!(
                    "{what} {task_id} is {}",
                    task.state.status()
                )));
            }
            if task.abort_requested {
                return Err(SessionError::message(format!(
                    "{what} {task_id} is abort-marked"
                )));
            }
        }
        Ok(())
    }

    async fn validate_task_replacements(&mut self) -> SessionResult<()> {
        let replacements: Vec<(TaskId, ConversationId)> = self
            .tasks
            .iter()
            .filter_map(|(id, task)| match &task.write {
                Some(write) if write.kind == TaskWriteKind::Replace => {
                    Some((*id, write.record.conversation_id))
                }
                _ => None,
            })
            .collect();
        for (id, conversation_id) in replacements {
            let committed = self.committed_task(id).await?;
            let Some(committed) = committed else {
                return Err(SessionError::message(format!("Task {id} does not exist")));
            };
            if is_terminal(&committed.state) {
                return Err(SessionError::message(format!(
                    "Task {id} is already terminal"
                )));
            }
            if committed.conversation_id != conversation_id {
                return Err(SessionError::message(format!(
                    "Task {id} cannot change conversations"
                )));
            }
        }
        Ok(())
    }

    async fn retire_terminal_task_documents(
        &mut self,
        plans: &mut Vec<DocumentPlan>,
    ) -> SessionResult<()> {
        let mut terminal_task_ids = std::collections::BTreeSet::new();
        for task in self.tasks.values() {
            if let Some(write) = &task.write
                && is_terminal(&write.record.state)
            {
                terminal_task_ids.insert(write.record.id);
            }
        }
        if terminal_task_ids.is_empty() {
            return Ok(());
        }
        let mut retiring = std::collections::BTreeSet::new();
        for plan in plans.iter_mut() {
            if let DocumentScope::Task { task_id } = plan.scope()
                && terminal_task_ids.contains(&task_id)
            {
                plan.retire = true;
                retiring.insert(plan.record_id());
            }
        }
        for task_id in terminal_task_ids {
            if self
                .tasks
                .get(&task_id)
                .and_then(|task| task.write.as_ref())
                .map(|write| write.kind == TaskWriteKind::Create)
                .unwrap_or(false)
            {
                continue;
            }
            let mut cursor = None;
            loop {
                let page = self
                    .host
                    .storage()
                    .scan_documents(
                        DocumentQuery {
                            scope: DocumentScope::Task { task_id },
                            at: DocumentPoint::CURRENT,
                            kind: None,
                        },
                        INTERNAL_SCAN_PAGE_SIZE,
                        cursor,
                        &self.context,
                    )
                    .await?;
                for record in page.items {
                    if !retiring.insert(record.id) {
                        continue;
                    }
                    let address = DocumentAddress {
                        kind: record.kind.clone(),
                        scope: record.scope.clone(),
                        key: record.key.clone(),
                    };
                    plans.push(DocumentPlan {
                        address_id: address_id(&address),
                        record: PlanRecord::Committed(record),
                        retire: true,
                        content: None,
                        change: None,
                        conversation_id: None,
                    });
                }
                cursor = page.next;
                if cursor.is_none() {
                    break;
                }
            }
        }
        Ok(())
    }

    async fn resolve_publication_owners(
        &mut self,
        plans: &mut [DocumentPlan],
    ) -> SessionResult<()> {
        for plan in plans.iter_mut() {
            if !publishes(plan) {
                continue;
            }
            match plan.scope() {
                DocumentScope::Conversation { conversation_id } => {
                    plan.conversation_id = Some(conversation_id);
                }
                DocumentScope::Task { task_id } => {
                    let cached = self
                        .tasks
                        .get(&task_id)
                        .and_then(|task| task.publication_conversation_id);
                    let conversation_id = match cached {
                        Some(id) => Some(id),
                        None => {
                            let current = self.current_task(task_id).await?;
                            let id = current.map(|task| task.conversation_id);
                            if let Some(id) = id {
                                self.tasks
                                    .entry(task_id)
                                    .or_default()
                                    .publication_conversation_id = Some(id);
                            }
                            id
                        }
                    };
                    plan.conversation_id = conversation_id;
                }
                DocumentScope::Session => {}
            }
        }
        Ok(())
    }

    async fn resolve_submission_changes(&mut self) -> SessionResult<()> {
        for (id, change) in &self.submission_changes {
            let current = match self.submissions.get(id) {
                Some(record) => record.clone(),
                None => self
                    .host
                    .storage()
                    .submission(*id, &self.context)
                    .await?
                    .ok_or_else(|| {
                        SessionError::message(format!("Submission {id} does not exist"))
                    })?,
            };
            let next = apply_submission_change(current, change.clone())?;
            self.submissions.insert(*id, next);
        }
        Ok(())
    }

    // ─── Helpers ────────────────────────────────────────────────────────────

    fn abort_changes(&mut self) {
        for document in &mut self.documents {
            if let Some(prepared) = &document.prepared {
                prepared.abort();
            } else if let Some(change) = &mut document.change {
                change.abort();
            }
        }
    }

    fn assert_open(&self) -> SessionResult<()> {
        if self.sealed {
            return Err(SessionError::message("Transaction has settled"));
        }
        Ok(())
    }
    fn assert_writable(&self) -> SessionResult<()> {
        self.assert_open()?;
        if self.host.read_only() {
            return Err(SessionError::ReadOnlySession("a transaction write"));
        }
        Ok(())
    }

    fn read(&self, method: &'static str) -> SessionResult<()> {
        self.assert_open()?;
        if self.has_table_write {
            return Err(StorageError::ReadAfterWrite(method).into());
        }
        Ok(())
    }

    fn assert_task_documents_open(&self, address: &DocumentAddress) -> SessionResult<()> {
        if let DocumentScope::Task { task_id } = &address.scope {
            let candidate = self
                .tasks
                .get(task_id)
                .and_then(|task| task.write.as_ref())
                .map(|write| &write.record.state);
            if candidate.map(is_terminal).unwrap_or(false) {
                return Err(SessionError::message(format!("Task {task_id} is terminal")));
            }
        }
        Ok(())
    }

    async fn require_conversation(&self, id: ConversationId) -> SessionResult<()> {
        if self.created_conversation_ids.contains(&id) {
            return Ok(());
        }
        if self
            .host
            .storage()
            .conversation(id, &self.context)
            .await?
            .is_none()
        {
            return Err(SessionError::message(format!(
                "Conversation {id} does not exist"
            )));
        }
        Ok(())
    }

    fn stamp_times(&self, value: TaskRecord, candidate: Option<&TaskRecord>) -> TaskRecord {
        let status = value.status();
        let started_at = candidate
            .and_then(|candidate| candidate.started_at)
            .or(value.started_at)
            .or_else(|| {
                if status == crate::types::TaskStatus::Running {
                    Some(self.host.now())
                } else {
                    None
                }
            });
        let ended_at = candidate
            .and_then(|candidate| candidate.ended_at)
            .or(value.ended_at)
            .or_else(|| {
                if status == crate::types::TaskStatus::Terminal {
                    Some(self.host.now())
                } else {
                    None
                }
            });
        if started_at == value.started_at && ended_at == value.ended_at {
            return value;
        }
        TaskRecord {
            started_at,
            ended_at,
            ..value
        }
    }

    async fn current_task(&mut self, id: TaskId) -> SessionResult<Option<TaskRecord>> {
        if let Some(write) = self.tasks.get(&id).and_then(|task| task.write.as_ref()) {
            return Ok(Some(write.record.clone()));
        }
        self.committed_task(id).await
    }

    async fn committed_task(&mut self, id: TaskId) -> SessionResult<Option<TaskRecord>> {
        if let Some(cached) = self
            .tasks
            .get(&id)
            .and_then(|task| task.committed_read.as_ref())
        {
            return Ok(cached.clone());
        }
        let loaded = self.host.storage().task(id, &self.context).await?;
        self.tasks.entry(id).or_default().committed_read = Some(loaded.clone());
        Ok(loaded)
    }

    fn latest_index(&self, address_id: &str) -> Option<usize> {
        self.latest_document_by_address.get(address_id).copied()
    }

    fn push_document(&mut self, entry: DocumentEntry) -> usize {
        let address_id = entry.address_id.clone();
        self.documents.push(entry);
        let index = self.documents.len() - 1;
        self.latest_document_by_address.insert(address_id, index);
        index
    }
}

/// Fields of one task created by [`Transaction::create_task`].
#[derive(Debug, Clone)]
pub struct TaskCreate {
    /// Registered task definition name.
    pub kind: String,
    /// Definition version.
    pub version: u64,
    /// Original task input.
    pub input: JsonValue,
    /// Initial durable checkpoint (`definition.initial(input)`).
    pub checkpoint: JsonValue,
    /// Ownership of the new task.
    pub ownership: TaskOwnership,
    /// Conversation when the task's conversation owns it.
    pub conversation_id: Option<ConversationId>,
    /// Whether a conversation-owned task is excluded from idle waits.
    pub background: bool,
}

pub(crate) fn doc_target<'a>(
    definition: &crate::documents::DocDefinition,
    owner: DocOwner<'a>,
) -> DocTarget<'a> {
    let _ = definition;
    match owner {
        DocOwner::Session => DocTarget::Session,
        DocOwner::Family(key) => DocTarget::SessionFamily(key),
        DocOwner::Conversation(conversation_id) => DocTarget::Conversation {
            conversation_id,
            key: None,
        },
        DocOwner::ConversationFamily(conversation_id, key) => DocTarget::Conversation {
            conversation_id,
            key: Some(key),
        },
        DocOwner::Task(task_id) => DocTarget::Task { task_id, key: None },
        DocOwner::TaskFamily(task_id, key) => DocTarget::Task {
            task_id,
            key: Some(key),
        },
    }
}

fn plan_document(document: &DocumentEntry) -> SessionResult<Option<DocumentPlan>> {
    let Some(target) = &document.target else {
        return Ok(None);
    };
    let address_id = document.address_id.clone();
    let retire = document.retire_on_commit;
    match target {
        DocumentTarget::Created {
            record,
            version,
            tracker,
        } => {
            let prepared = document
                .prepared
                .clone()
                .ok_or_else(|| SessionError::message("Created document was not prepared"))?;
            let value = prepared.value().as_object().cloned().unwrap_or_default();
            Ok(Some(DocumentPlan {
                address_id,
                record: PlanRecord::Create(record.clone()),
                retire,
                content: Some(StorageWrite::DocumentCreate {
                    record: record.clone(),
                    content: crate::types::DocumentContent::Base {
                        version: *version,
                        value,
                    },
                }),
                change: Some(PlanChange {
                    tracker: TrackerRef::Created(Arc::clone(tracker)),
                    prepared,
                    version: *version,
                    loaded: None,
                    definition: document.definition.clone(),
                }),
                conversation_id: None,
            }))
        }
        DocumentTarget::ForkCopy { record, source } => Ok(Some(DocumentPlan {
            address_id,
            record: PlanRecord::Create(record.clone()),
            retire,
            content: Some(StorageWrite::DocumentCopy {
                record: record.clone(),
                source: source.clone(),
            }),
            change: None,
            conversation_id: None,
        })),
        DocumentTarget::RetireOnly { record } => Ok(Some(DocumentPlan {
            address_id,
            record: PlanRecord::Committed(record.clone()),
            retire: true,
            content: None,
            change: None,
            conversation_id: None,
        })),
        DocumentTarget::Loaded(loaded) => {
            let definition = document
                .definition
                .clone()
                .ok_or_else(|| SessionError::message("Loaded document lost its definition"))?;
            let prepared = document
                .prepared
                .clone()
                .ok_or_else(|| SessionError::message("Loaded document was not prepared"))?;
            let version = definition.version;
            let id = loaded.record.id;
            let content = if loaded.stored_version() < version {
                Some(StorageWrite::DocumentChange {
                    id,
                    content: crate::types::DocumentContent::Base {
                        version,
                        value: prepared.value().as_object().cloned().unwrap_or_default(),
                    },
                })
            } else if !prepared.ops().is_empty() {
                Some(StorageWrite::DocumentChange {
                    id,
                    content: crate::types::DocumentContent::Delta {
                        version,
                        ops: prepared.ops().to_vec(),
                    },
                })
            } else {
                None
            };
            Ok(Some(DocumentPlan {
                address_id,
                record: PlanRecord::Committed(loaded.record.clone()),
                retire,
                content,
                change: Some(PlanChange {
                    tracker: TrackerRef::Loaded(Arc::clone(loaded)),
                    prepared,
                    version,
                    loaded: Some(Arc::clone(loaded)),
                    definition: Some(definition),
                }),
                conversation_id: None,
            }))
        }
    }
}

fn reject_fork_source_writes(
    plans: &[DocumentPlan],
    source_documents: &std::collections::BTreeSet<DocumentId>,
    source_conversations: &std::collections::BTreeSet<ConversationId>,
) -> SessionResult<()> {
    for plan in plans {
        if plan.content.is_none() && !plan.retire {
            continue;
        }
        let record_id = plan.record_id();
        if source_documents.contains(&record_id) {
            return Err(SessionError::message(format!(
                "Cannot change fork source document {record_id} in the fork transaction"
            )));
        }
        if let DocumentScope::Conversation { conversation_id } = plan.scope()
            && source_conversations.contains(&conversation_id)
            && plan.fork() == Some(crate::types::DocumentFork::Current)
        {
            return Err(SessionError::message(format!(
                "Cannot fork conversation {conversation_id} while changing its current-policy documents"
            )));
        }
    }
    Ok(())
}

fn apply_submission_change(
    current: SubmissionRecord,
    change: SubmissionChange,
) -> SessionResult<SubmissionRecord> {
    use crate::types::{InputSubmissionStatus, WriteSubmissionStatus};
    let settled = match &current {
        SubmissionRecord::Input { status, .. } => matches!(
            status,
            InputSubmissionStatus::Done { .. } | InputSubmissionStatus::Unanswered { .. }
        ),
        SubmissionRecord::Write { status, .. } => {
            matches!(
                status,
                WriteSubmissionStatus::Done { .. } | WriteSubmissionStatus::Unanswered { .. }
            )
        }
    };
    if settled {
        return Ok(current);
    }
    match change {
        SubmissionChange::Placed(entry) => match current {
            SubmissionRecord::Input {
                id,
                conversation_id,
                request_id,
                status: InputSubmissionStatus::Queued,
            } => Ok(SubmissionRecord::Input {
                id,
                conversation_id,
                request_id,
                status: InputSubmissionStatus::Placed { entry },
            }),
            SubmissionRecord::Write {
                id,
                conversation_id,
                request_id,
                status: WriteSubmissionStatus::Queued,
            } => Ok(SubmissionRecord::Write {
                id,
                conversation_id,
                request_id,
                status: WriteSubmissionStatus::Done { entry },
            }),
            other => Err(SessionError::message(format!(
                "Submission {} is not queued",
                other.id()
            ))),
        },
        SubmissionChange::Settled(settlement) => match (current, settlement) {
            (
                SubmissionRecord::Input {
                    id,
                    conversation_id,
                    request_id,
                    status: InputSubmissionStatus::Placed { entry },
                },
                SubmissionSettlement::Done { answer },
            ) => Ok(SubmissionRecord::Input {
                id,
                conversation_id,
                request_id,
                status: InputSubmissionStatus::Done { entry, answer },
            }),
            (
                SubmissionRecord::Input {
                    id,
                    conversation_id,
                    request_id,
                    status,
                },
                SubmissionSettlement::Unanswered { reason, detail },
            ) => {
                let entry = match status {
                    InputSubmissionStatus::Placed { entry } => Some(entry),
                    _ => None,
                };
                Ok(SubmissionRecord::Input {
                    id,
                    conversation_id,
                    request_id,
                    status: InputSubmissionStatus::Unanswered {
                        entry,
                        reason,
                        detail,
                    },
                })
            }
            (
                SubmissionRecord::Write {
                    id,
                    conversation_id,
                    request_id,
                    ..
                },
                SubmissionSettlement::Unanswered { reason, detail },
            ) => Ok(SubmissionRecord::Write {
                id,
                conversation_id,
                request_id,
                status: WriteSubmissionStatus::Unanswered { reason, detail },
            }),
            (record, _) => Err(SessionError::message(format!(
                "Submission {} is not a placed input",
                record.id()
            ))),
        },
    }
}

fn is_terminal(state: &TaskState) -> bool {
    matches!(state, TaskState::Terminal { .. })
}

fn is_completing(state: &TaskState) -> bool {
    matches!(state, TaskState::Completing { .. })
}
