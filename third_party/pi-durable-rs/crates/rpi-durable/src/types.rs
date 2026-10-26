//! Durable data model and storage contract (port of the storage-relevant
//! sections of `packages/durable/src/types.ts` @ `abe508e1b`).
//!
//! The minimal record set is exactly what the shared conformance suite and the
//! three backends compile and run against (V17-13 §8-5): conversation, entry,
//! task, submission and document records, their query/page envelopes, the
//! `StorageWrite` batch, and the `Storage` trait. Session/Tx/harness surfaces
//! arrive with V17-14.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::chord::delta::Op;
use crate::chord::json::{JsonObject, JsonValue};
use crate::errors::{StorageError, StorageResult};
use crate::ids::{ConversationId, DocumentId, EntryId, Id, Seq, SubmissionId, TaskId};

/// `Message` is reused from rpi-ai (upstream `types.ts:3` type-only import).
pub use rpi_ai::types::Message;

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// Fork source and inclusive parent entry through which history is inherited.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationParent {
    pub conversation_id: ConversationId,
    pub at: EntryId,
}

/// Creator edge used for attribution, subtree abort, and subtree idle waits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationOwner {
    pub conversation_id: ConversationId,
    pub task_id: TaskId,
}

/// Immutable identity, history ancestry, and task ownership of a transcript scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRecord {
    pub id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ConversationParent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<ConversationOwner>,
}

/// An immutable override of one visible entry's contribution to model context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEdit {
    /// Entry whose model messages are omitted or replaced.
    pub target: EntryId,
    #[serde(flatten)]
    pub action: ContextEditAction,
}

/// Action of a [`ContextEdit`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum ContextEditAction {
    /// Omit the target's model messages.
    Omit,
    /// Replace the target's model messages.
    Replace {
        /// Messages contributed instead.
        messages: Vec<Message>,
    },
}

/// Immutable transcript event with separate model-facing and
/// application-facing payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryRecord {
    pub id: EntryId,
    pub conversation_id: ConversationId,
    /// Application-defined entry discriminator.
    pub kind: String,
    /// Messages contributed to model context; absent for display or bookkeeping entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Vec<Message>>,
    /// JSON payload consumed by views, extensions, or bookkeeping logic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
    /// First entry in the active context selected by this entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<EntryId>,
    /// Context-only overrides of earlier visible entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<Vec<ContextEdit>>,
    /// Task that appended this entry, when it was produced by durable work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_task_id: Option<TaskId>,
}

impl EntryRecord {
    /// An entry with the given identity and kind and no optional payloads.
    pub fn new(id: EntryId, conversation_id: ConversationId, kind: impl Into<String>) -> Self {
        Self {
            id,
            conversation_id,
            kind: kind.into(),
            model: None,
            data: None,
            head: None,
            edits: None,
            by_task_id: None,
        }
    }
}

/// How a waiting task treats the tasks it waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JoinPolicy {
    /// Fail as soon as one waited task fails.
    FailFast,
    /// Wait for every waited task to settle.
    AllSettled,
}

/// JSON-safe error snapshot persisted instead of a runtime `Error` object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskOutcomeError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<JsonValue>,
}

/// Durable reason and optional result recorded when a task becomes terminal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TaskOutcome {
    /// Task completed with a durable result.
    Completed {
        /// Task result.
        result: JsonValue,
    },
    /// Expected task or domain failure explicitly committed by its implementation.
    Failed {
        /// Failure snapshot.
        error: TaskOutcomeError,
        /// Optional partial result retained by the implementation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<JsonValue>,
    },
    /// Explicit cancellation handled by the task's abort protocol.
    Aborted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<JsonValue>,
    },
    /// Task that cannot resume because its definition or migration is unavailable.
    Orphaned {
        /// Why the task is orphaned.
        reason: String,
    },
    /// Runtime-detected contract failure.
    Faulted {
        /// Failure snapshot.
        error: TaskOutcomeError,
    },
}

/// Complete durable execution state of a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TaskState {
    /// Eligible for scheduling.
    Pending {
        /// Complete durable state from which execution resumes.
        checkpoint: JsonValue,
    },
    /// Reserved by one in-memory task invocation.
    Running {
        /// Complete durable state from which execution resumes.
        checkpoint: JsonValue,
    },
    /// Asleep without an invocation until the Harness clock reaches `until`.
    Sleeping {
        /// Complete durable state from which execution resumes.
        checkpoint: JsonValue,
        /// Deadline on the Harness clock, in epoch milliseconds.
        until: f64,
    },
    /// Parked until every task in `on` is terminal.
    Waiting {
        /// Complete durable state from which execution resumes.
        checkpoint: JsonValue,
        /// Tasks this task waits on.
        on: Vec<TaskId>,
        /// How the wait reacts to failures.
        policy: JoinPolicy,
    },
    /// Outcome decided; runs no more code.
    Completing {
        /// Decided outcome.
        outcome: TaskOutcome,
    },
    /// Permanently settled durable result receipt.
    Terminal {
        /// Final outcome.
        outcome: TaskOutcome,
    },
}

impl TaskState {
    /// The persisted status discriminator.
    pub fn status(&self) -> &'static str {
        match self {
            TaskState::Pending { .. } => "pending",
            TaskState::Running { .. } => "running",
            TaskState::Waiting { .. } => "waiting",
            TaskState::Sleeping { .. } => "sleeping",
            TaskState::Completing { .. } => "completing",
            TaskState::Terminal { .. } => "terminal",
        }
    }
}

/// Serialized task status accepted by scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    /// Pending.
    Pending,
    /// Running.
    Running,
    /// Waiting.
    Waiting,
    /// Sleeping.
    Sleeping,
    /// Completing.
    Completing,
    /// Terminal.
    Terminal,
}

impl TaskStatus {
    /// Stable SQL/JSON text.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Waiting => "waiting",
            TaskStatus::Sleeping => "sleeping",
            TaskStatus::Completing => "completing",
            TaskStatus::Terminal => "terminal",
        }
    }
}

/// Complete replacement record for one durable task state machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    pub id: TaskId,
    pub conversation_id: ConversationId,
    /// Registered task definition name.
    pub kind: String,
    /// Definition version used to migrate live input and checkpoints.
    pub version: u64,
    /// Original task input retained while the task is live or terminal.
    pub input: JsonValue,
    /// Owning task of a child task; absent for a task its conversation owns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<TaskId>,
    /// Whether this conversation-owned task is excluded from ordinary idle waits.
    pub background: bool,
    /// Durable abort mark checked before run-mode progress is committed.
    pub abort_requested: bool,
    /// Wall-clock milliseconds of the first change to `running`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    /// Wall-clock milliseconds of the change to `terminal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<f64>,
    /// Current state machine state.
    pub state: TaskState,
    /// Small first-writer-wins values retained while the task can run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memos: Option<JsonObject>,
}

impl TaskRecord {
    /// The status discriminator of the current state.
    pub fn status(&self) -> TaskStatus {
        match self.state {
            TaskState::Pending { .. } => TaskStatus::Pending,
            TaskState::Running { .. } => TaskStatus::Running,
            TaskState::Waiting { .. } => TaskStatus::Waiting,
            TaskState::Sleeping { .. } => TaskStatus::Sleeping,
            TaskState::Completing { .. } => TaskStatus::Completing,
            TaskState::Terminal { .. } => TaskStatus::Terminal,
        }
    }
}

/// Lifecycle state of a queued or placed input submission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum InputSubmissionStatus {
    /// Admitted but not yet represented in the transcript.
    Queued,
    /// Added to the transcript and owned by an active run.
    Placed {
        /// Placed entry.
        entry: EntryId,
    },
    /// Successfully answered user input.
    Done {
        /// Placed entry.
        entry: EntryId,
        /// Answer entry.
        answer: EntryId,
    },
    /// Terminal input that can no longer receive an answer.
    Unanswered {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry: Option<EntryId>,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<JsonValue>,
    },
}

/// Lifecycle state of a passive write submission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum WriteSubmissionStatus {
    /// Admitted but not yet appended to the transcript.
    Queued,
    /// Successfully appended passive entry.
    Done {
        /// Appended entry.
        entry: EntryId,
    },
    /// Terminal passive write that could not be placed.
    Unanswered {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<JsonValue>,
    },
}

/// Persisted lifecycle record for one admitted submission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SubmissionRecord {
    /// User input submission.
    Input {
        /// Identity.
        id: SubmissionId,
        /// Conversation the submission belongs to.
        conversation_id: ConversationId,
        /// Host-provided deduplication key, scoped to the conversation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(flatten)]
        status: InputSubmissionStatus,
    },
    /// Passive write submission.
    Write {
        /// Identity.
        id: SubmissionId,
        /// Conversation the submission belongs to.
        conversation_id: ConversationId,
        /// Host-provided deduplication key, scoped to the conversation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(flatten)]
        status: WriteSubmissionStatus,
    },
}

impl SubmissionRecord {
    /// Identity.
    pub fn id(&self) -> SubmissionId {
        match self {
            SubmissionRecord::Input { id, .. } | SubmissionRecord::Write { id, .. } => *id,
        }
    }

    /// Conversation.
    pub fn conversation_id(&self) -> ConversationId {
        match self {
            SubmissionRecord::Input {
                conversation_id, ..
            }
            | SubmissionRecord::Write {
                conversation_id, ..
            } => *conversation_id,
        }
    }

    /// Deduplication key.
    pub fn request_id(&self) -> Option<&str> {
        match self {
            SubmissionRecord::Input { request_id, .. }
            | SubmissionRecord::Write { request_id, .. } => request_id.as_deref(),
        }
    }

    /// Status discriminator (`queued`/`placed`/`done`/`unanswered`).
    pub fn status(&self) -> &'static str {
        match self {
            SubmissionRecord::Input { status, .. } => match status {
                InputSubmissionStatus::Queued => "queued",
                InputSubmissionStatus::Placed { .. } => "placed",
                InputSubmissionStatus::Done { .. } => "done",
                InputSubmissionStatus::Unanswered { .. } => "unanswered",
            },
            SubmissionRecord::Write { status, .. } => match status {
                WriteSubmissionStatus::Queued => "queued",
                WriteSubmissionStatus::Done { .. } => "done",
                WriteSubmissionStatus::Unanswered { .. } => "unanswered",
            },
        }
    }
}

/// Serialized submission status accepted by scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubmissionStatusFilter {
    /// Queued.
    Queued,
    /// Placed.
    Placed,
    /// Done.
    Done,
    /// Unanswered.
    Unanswered,
}

impl SubmissionStatusFilter {
    /// Stable SQL/JSON text.
    pub fn as_str(self) -> &'static str {
        match self {
            SubmissionStatusFilter::Queued => "queued",
            SubmissionStatusFilter::Placed => "placed",
            SubmissionStatusFilter::Done => "done",
            SubmissionStatusFilter::Unanswered => "unanswered",
        }
    }
}

/// Whether a conversation document keeps history or only current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocumentHistory {
    /// Retain only current state.
    Latest,
    /// Retain history needed for as-of reads.
    Rewindable,
}

/// How a conversation document is initialized when forked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocumentFork {
    /// Initialize at the fork's cutoff.
    AsOf,
    /// Initialize from current source state.
    Current,
    /// Initialize from the definition's initial value.
    Initial,
}

/// Document scope (upstream `DocumentRecord["scope"]`); conversation history
/// and fork semantics live on the record itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DocumentScope {
    /// Session-scoped singleton.
    Session,
    /// Conversation-scoped singleton or family member.
    Conversation {
        /// Owning conversation.
        conversation_id: ConversationId,
    },
    /// Task-scoped singleton or family member.
    Task {
        /// Owning task.
        task_id: TaskId,
    },
}

/// Fields supplied when storage creates and stamps a new document record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentCreate {
    /// Unique incarnation ID.
    pub id: DocumentId,
    /// Stable document definition kind.
    pub kind: String,
    /// Family member key; absent for singleton documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Logical scope.
    pub scope: DocumentScope,
    /// Conversation history retention; required for conversation scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<DocumentHistory>,
    /// Conversation fork initialization; required for conversation scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<DocumentFork>,
}

/// Persisted lifecycle record for one create-to-retire document incarnation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRecord {
    /// Unique incarnation ID.
    pub id: DocumentId,
    /// Stable document definition kind.
    pub kind: String,
    /// Family member key; absent for singleton documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Commit that created the incarnation, stamped by storage.
    pub created_at: Seq,
    /// Commit that retired the incarnation; absent while it is current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<Seq>,
    /// Logical scope.
    pub scope: DocumentScope,
    /// Conversation history retention; required for conversation scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<DocumentHistory>,
    /// Conversation fork initialization; required for conversation scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<DocumentFork>,
}

/// Detached materialized value and stored definition version at a selected point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredDocument {
    /// Incarnation record.
    pub record: DocumentRecord,
    /// Definition version of `value`.
    pub version: u64,
    /// Materialized value.
    pub value: JsonObject,
    /// Deltas replayed after the selected base to materialize `value`.
    pub deltas_since_base: u64,
}

/// Complete checkpoint or Chord operation batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DocumentContent {
    /// A definition-version checkpoint.
    Base {
        /// Definition version.
        version: u64,
        /// Root object.
        value: JsonObject,
    },
    /// Operations applied over the previous revision of the same version.
    Delta {
        /// Definition version.
        version: u64,
        /// Operation batch.
        ops: Vec<Op>,
    },
}

impl DocumentContent {
    /// Definition version.
    pub fn version(&self) -> u64 {
        match self {
            DocumentContent::Base { version, .. } | DocumentContent::Delta { version, .. } => {
                *version
            }
        }
    }

    /// Whether this is a base checkpoint.
    pub fn is_base(&self) -> bool {
        matches!(self, DocumentContent::Base { .. })
    }
}

/// Exact persisted source selected for a definition-free document copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentCopySource {
    /// Source incarnation.
    pub id: DocumentId,
    /// Selected point.
    pub at: DocumentPoint,
}

/// Current state or one historical commit sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DocumentPoint {
    /// One historical commit sequence.
    At(Seq),
    /// The current state.
    Current(CurrentMarker),
}

/// Marker value deserialized from `"current"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CurrentMarker {
    /// The literal `"current"`.
    #[serde(rename = "current")]
    Current,
}

impl DocumentPoint {
    /// The current point.
    pub const CURRENT: Self = Self::Current(CurrentMarker::Current);

    /// Whether this selects current state.
    pub fn is_current(self) -> bool {
        matches!(self, Self::Current(_))
    }

    /// The sequence bound (`u64::MAX` for current).
    pub fn upper(self) -> u64 {
        match self {
            Self::At(seq) => seq.0,
            Self::Current(_) => u64::MAX,
        }
    }
}

impl From<Seq> for DocumentPoint {
    fn from(value: Seq) -> Self {
        Self::At(value)
    }
}

impl std::fmt::Display for DocumentPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::At(seq) => write!(f, "{seq}"),
            Self::Current(_) => write!(f, "current"),
        }
    }
}

/// Exact logical identity of a singleton or one keyed family member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentAddress {
    /// Stable definition kind.
    pub kind: String,
    /// Logical scope.
    pub scope: DocumentScope,
    /// Absent selects the singleton; present selects one family member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// One record or document mutation in an atomic storage commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
#[allow(clippy::large_enum_variant)] // persisted wire shape; boxing would change it
pub enum StorageWrite {
    /// Insert or replace a conversation.
    #[serde(rename = "conversation")]
    Conversation {
        /// Full record.
        value: ConversationRecord,
    },
    /// Insert an entry (append-only).
    #[serde(rename = "entry")]
    Entry {
        /// Full record.
        value: EntryRecord,
    },
    /// Insert or replace a task record.
    #[serde(rename = "task")]
    Task {
        /// Full record.
        value: TaskRecord,
    },
    /// Insert or replace a submission record.
    #[serde(rename = "submission")]
    Submission {
        /// Full record.
        value: SubmissionRecord,
    },
    /// Create a document incarnation from a base checkpoint.
    #[serde(rename = "document.create")]
    DocumentCreate {
        /// Incarnation record to stamp.
        record: DocumentCreate,
        /// Base content.
        content: DocumentContent,
    },
    /// Copy a stored document base into a new conversation incarnation.
    #[serde(rename = "document.copy")]
    DocumentCopy {
        /// Incarnation record to stamp.
        record: DocumentCreate,
        /// Exact persisted source.
        source: DocumentCopySource,
    },
    /// Append a checkpoint or delta to an incarnation.
    #[serde(rename = "document.change")]
    DocumentChange {
        /// Target incarnation.
        id: DocumentId,
        /// Content batch.
        content: DocumentContent,
    },
    /// Retire an incarnation.
    #[serde(rename = "document.retire")]
    DocumentRetire {
        /// Target incarnation.
        id: DocumentId,
    },
}

impl StorageWrite {
    /// The claimed record ID, if any (document change/retire carry none).
    pub fn write_id(&self) -> Option<u64> {
        match self {
            StorageWrite::Conversation { value } => Some(value.id.get()),
            StorageWrite::Entry { value } => Some(value.id.get()),
            StorageWrite::Task { value } => Some(value.id.get()),
            StorageWrite::Submission { value } => Some(value.id().get()),
            StorageWrite::DocumentCreate { record, .. }
            | StorageWrite::DocumentCopy { record, .. } => Some(record.id.get()),
            StorageWrite::DocumentChange { .. } | StorageWrite::DocumentRetire { .. } => None,
        }
    }

    /// Record-table name claimed by this write (upstream `TableName`).
    pub fn table_name(&self) -> Option<&'static str> {
        match self {
            StorageWrite::Conversation { .. } => Some("conversation"),
            StorageWrite::Entry { .. } => Some("entry"),
            StorageWrite::Task { .. } => Some("task"),
            StorageWrite::Submission { .. } => Some("submission"),
            StorageWrite::DocumentCreate { .. } | StorageWrite::DocumentCopy { .. } => {
                Some("document")
            }
            StorageWrite::DocumentChange { .. } | StorageWrite::DocumentRetire { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Queries and pages
// ---------------------------------------------------------------------------

/// IDs in `ascending` (oldest first) or `descending` (newest first) order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScanOrder {
    /// Oldest first.
    Ascending,
    /// Newest first.
    Descending,
}

impl ScanOrder {
    /// SQL direction text.
    pub fn direction(self) -> &'static str {
        match self {
            ScanOrder::Ascending => "ASC",
            ScanOrder::Descending => "DESC",
        }
    }

    /// Whether this is the ascending order.
    pub fn is_ascending(self) -> bool {
        matches!(self, ScanOrder::Ascending)
    }
}

/// Backend-owned continuation state callers only round-trip to the same scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cursor {
    /// Last returned ID.
    pub after: u64,
    /// Order the scan was created with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<ScanOrder>,
}

impl Cursor {
    /// Continuation after `id` in `order` (upstream `nextCursor`).
    pub fn after_id(id: u64, order: ScanOrder) -> Self {
        Self {
            after: id,
            order: Some(order),
        }
    }
}

/// One ordered scan result and its optional continuation state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    /// Items in scan order.
    pub items: Vec<T>,
    /// Continuation when more items remain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
}

/// Optional filters for an ordered conversation scan.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConversationQuery {
    /// Only conversations owned by this conversation.
    pub owner_conversation_id: Option<ConversationId>,
    /// Only conversations owned by this task.
    pub owner_task_id: Option<TaskId>,
    /// Default ascending; a cursor continues the order it was created with.
    pub order: Option<ScanOrder>,
}

/// Inclusive ID bounds for a scan of one conversation's fork-aware history.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryQuery {
    /// Conversation whose visible history is scanned.
    pub conversation_id: ConversationId,
    /// Oldest entry ID that may be returned.
    pub min_entry_id: Option<EntryId>,
    /// Newest entry ID that may be returned.
    pub max_entry_id: Option<EntryId>,
    /// Default descending; a cursor continues the order it was created with.
    pub order: Option<ScanOrder>,
}

/// Optional filters for an ordered scan of durable task records.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskQuery {
    /// Only tasks of this conversation.
    pub conversation_id: Option<ConversationId>,
    /// Only tasks of this kind.
    pub kind: Option<String>,
    /// Only tasks in this status.
    pub status: Option<TaskStatus>,
    /// Only tasks with this abort mark.
    pub abort_requested: Option<bool>,
    /// Only tasks with this background mark.
    pub background: Option<bool>,
    /// Default ascending; a cursor continues the order it was created with.
    pub order: Option<ScanOrder>,
}

/// Optional filters for an ordered scan of submission records.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SubmissionQuery {
    /// Only submissions of this conversation.
    pub conversation_id: Option<ConversationId>,
    /// Only submissions in this status.
    pub status: Option<SubmissionStatusFilter>,
    /// Default ascending; a cursor continues the order it was created with.
    pub order: Option<ScanOrder>,
}

/// Ordered scan of document incarnations alive in one exact scope at one point.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentQuery {
    /// Exact scope.
    pub scope: DocumentScope,
    /// Selected point.
    pub at: DocumentPoint,
    /// Only incarnations of this kind.
    pub kind: Option<String>,
}

/// The latest committed record for an entry and the sequence that persisted it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEntry {
    /// Entry record.
    pub entry: EntryRecord,
    /// Commit sequence.
    pub commit_seq: Seq,
}

// ---------------------------------------------------------------------------
// Storage trait
// ---------------------------------------------------------------------------

/// Async durable storage contract (upstream `Storage`, `types.ts:1015`).
///
/// The trait is object safe so the shared conformance suite can run any backend
/// behind `Arc<dyn Storage>`; typed ID minting lives on [`StorageExt`].
#[async_trait]
pub trait Storage: Send + Sync {
    /// Atomically persist one batch and return its sequence. Once resolved,
    /// later reads through this storage observe it.
    async fn commit(
        &self,
        writes: &[StorageWrite],
        context: &crate::chord::context::Context,
    ) -> StorageResult<Seq>;

    /// Return a fresh ID from the session-global numeric namespace.
    async fn mint_id(&self) -> StorageResult<u64>;

    /// Look up one conversation by exact ID.
    async fn conversation(
        &self,
        id: ConversationId,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Option<ConversationRecord>>;

    /// Scan conversations in `query.order` (default ascending) by ID.
    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Page<ConversationRecord>>;

    /// Look up one global entry and the sequence of the commit that persisted it.
    async fn entry(
        &self,
        id: EntryId,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Option<StoredEntry>>;

    /// Look up one entry only when it is visible through the requested
    /// conversation's ancestry.
    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Option<StoredEntry>>;

    /// Newest visible entry with a `head` at or below the optional inclusive cutoff.
    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Option<EntryRecord>>;

    /// Scan the inclusive visible range in `query.order` (default descending).
    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Page<EntryRecord>>;

    /// Look up the latest complete record for one task.
    async fn task(
        &self,
        id: TaskId,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Option<TaskRecord>>;

    /// Scan task records matching every supplied filter.
    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Page<TaskRecord>>;

    /// Look up the latest complete record for one admitted submission.
    async fn submission(
        &self,
        id: SubmissionId,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Option<SubmissionRecord>>;

    /// Scan submissions matching every supplied filter.
    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Page<SubmissionRecord>>;

    /// Find a submission by its conversation-scoped host deduplication key.
    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Option<SubmissionRecord>>;

    /// Resolve the incarnation occupying one exact logical address at the selected point.
    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Option<DocumentRecord>>;

    /// Materialize one specific incarnation by ID at the selected point.
    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Option<StoredDocument>>;

    /// Scan incarnations alive in one exact scope at the selected point.
    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &crate::chord::context::Context,
    ) -> StorageResult<Page<DocumentRecord>>;

    /// Release backend resources; all later operations must reject.
    async fn close(&self, context: &crate::chord::context::Context) -> StorageResult<()>;
}

/// Typed helpers layered over [`Storage`] (upstream generic `mintId<I>()`).
#[async_trait]
pub trait StorageExt: Storage {
    /// Mint an ID branded for record kind `T`.
    async fn mint_id_as<T>(&self) -> StorageResult<Id<T>> {
        Ok(Id::from_number(self.mint_id().await?))
    }
}

#[async_trait]
impl<S: Storage + ?Sized> StorageExt for S {}

/// The error type surfaced by the storage contract.
pub type Error = StorageError;
