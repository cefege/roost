//! Session kernel of the durable runtime (port of the session domain of
//! `packages/durable/src/session/*` @ `abe508e1b`: `session.ts`, `transaction.ts`,
//! `observation.ts`, `forks.ts`).
//!
//! The kernel owns one **mutation line**: every commit callback, storage
//! settlement, adoption and publication runs serialized while the line is held;
//! only committed state is observable. This module root carries the shared error
//! surface and the public value types; [`session`] carries the single-line
//! [`session::SessionImpl`], [`transaction`] the staging [`transaction::Transaction`],
//! [`observation`] the async watch/state bridge, and [`forks`] the conversation
//! document copy planner.
//!
//! Upstream is a JavaScript implementation built on promises and microtasks; the
//! Rust port maps the line to a `tokio::sync::Mutex`, callbacks to boxed futures,
//! and `Draft<T>` proxies to [`crate::chord::tracker::DocCell`] (registered shape
//! differences, V17-14 §7.3).

use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

use crate::chord::delta::{DeltaError, Op};
use crate::chord::json::{JsonObject, JsonValue};
use crate::documents::DocumentError;
use crate::errors::StorageError;
use crate::ids::{ConversationId, Seq, TaskId};
use crate::types::{DocumentCopySource, DocumentRecord, StorageWrite};

pub mod forks;
pub mod observation;
// The design keeps the kernel in `session/session.rs` (frozen layout); Clippy's
// `module_inception` is a style lint for the resulting `session::session` path.
#[allow(clippy::module_inception)]
pub mod session;
mod storage_guard;
pub mod transaction;

pub use forks::{ForkDocumentCopy, prepare_fork_document_copies};
pub use observation::{
    CommittedStateSource, CommittedWatch, MAX_PENDING_WATCH_FRAMES, ObservedDocumentValue,
    RETIREMENT_OPERATIONS, WatchEnd, WatchFuture, WatchListener,
};
pub use session::{
    CloseListener, CommitFn, CommitListener, CommitOptions, CommitPersistence,
    ConversationCreatedHook, ConversationDocumentView, NowFn, SessionImpl, SessionOptions,
    commit_fn, create_session, watch_listener,
};
pub use transaction::{
    ConversationOwnership, DocDraft, EntryDraft, EntryHead, HoldableDocument, LoadedDocument,
    SubmissionCreate, SubmissionSettlement, TaskCreate, TaskOwnership, Transaction,
    TransactionScope,
};

/// An idempotent unsubscribe handle (upstream's returned unsubscribe function).
pub struct Subscription {
    unsubscribe: std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Subscription {
    pub(crate) fn new(unsubscribe: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            unsubscribe: std::sync::Mutex::new(Some(Box::new(unsubscribe))),
        }
    }

    /// Stop future callbacks. Idempotent.
    pub fn unsubscribe(&self) {
        if let Ok(mut slot) = self.unsubscribe.lock()
            && let Some(unsubscribe) = slot.take()
        {
            unsubscribe();
        }
    }
}

/// A boxed session future; every public session operation that is not a bare
/// accessor returns one of these so callers can `.await` uniformly.
pub type SessionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SessionError>> + Send + 'a>>;

/// Failure surface of the session kernel.
///
/// Upstream throws plain `Error` subclasses and branches on `instanceof
/// StorageRejected`; the Rust port preserves the message text and keeps the
/// storage error reachable through [`SessionError::Storage`] so
/// [`StorageError::ReadAfterWrite`] and [`StorageError::Rejected`] stay
/// inspectable.
#[derive(Debug, Error)]
pub enum SessionError {
    /// Admission is sealed; every later operation rejects.
    #[error("Session is closed")]
    Closed,
    /// A read-only Harness refuses writes and task execution.
    #[error("Read-only Harness: {0} refused")]
    ReadOnlySession(&'static str),
    /// The session failed after the first unexpected storage failure.
    #[error("Session failed: {0}")]
    SessionFailed(String),
    /// Storage failure surfaced unchanged to the first failing caller.
    #[error(transparent)]
    Storage(StorageError),
    #[error(transparent)]
    Document(#[from] DocumentError),
    /// Delta application failure.
    #[error(transparent)]
    Delta(#[from] DeltaError),
    /// Caller cancellation observed before admission.
    #[error("The operation was aborted")]
    Aborted,
    /// Transaction, validation, or callback failure carrying the upstream text.
    #[error("{0}")]
    Message(String),
}

impl SessionError {
    /// Build a message error with the upstream text.
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }

    /// Build a callback failure error with the caller's message.
    pub fn callback(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}
impl From<StorageError> for SessionError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::SessionFailed(cause) => Self::SessionFailed(cause),
            error => Self::Storage(error),
        }
    }
}

/// Result alias for session operations.
pub type SessionResult<T> = Result<T, SessionError>;

/// Owner argument of a typed document access (upstream `doc`/`snapshot` overload
/// arguments).
///
/// The frozen design listed `{ Session, Conversation, Task, Family }`; the Rust
/// port adds explicit family-member variants because a conversation/task-scoped
/// family member needs both its scope owner and its key (shape difference: the
/// frozen enum could not express those overloads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocOwner<'a> {
    /// Session-scoped singleton.
    Session,
    /// Session-scoped family member.
    Family(&'a str),
    /// Conversation-scoped singleton.
    Conversation(ConversationId),
    /// Conversation-scoped family member.
    ConversationFamily(ConversationId, &'a str),
    /// Task-scoped singleton.
    Task(TaskId),
    /// Task-scoped family member.
    TaskFamily(TaskId, &'a str),
}

/// One table or document change carried by a [`CommitPublication`].
#[derive(Debug, Clone, PartialEq)]
pub enum CommitChange {
    /// A table write (`conversation`/`entry`/`task`/`submission`), in staging order.
    Table(StorageWrite),
    /// A document content change (create/change) or incarnation retirement.
    Document(DocumentChange),
    /// A definition-free conversation document copy.
    DocumentCopy(DocumentCopyChange),
}

/// One committed document content change or retirement (upstream
/// `DocumentCommitChange` with `type: "document"`).
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentChange {
    /// Incarnation record after the commit; `retired_at` is stamped on retirement.
    pub record: DocumentRecord,
    /// Publication conversation; absent for session-scoped documents.
    pub conversation_id: Option<ConversationId>,
    /// Definition version of `value`; absent for a retirement.
    pub version: Option<u64>,
    /// Committed value; absent for a retirement.
    pub value: Option<JsonObject>,
    /// Exact operation batch that produced `value`; empty for a creation or a
    /// migration-only base.
    pub ops: Vec<Op>,
}

impl DocumentChange {
    /// Whether this frame retires the incarnation.
    pub fn is_retirement(&self) -> bool {
        self.version.is_none()
    }

    /// The frame value as strict JSON (`null` for a retirement).
    pub fn frame_value(&self) -> JsonValue {
        match &self.value {
            None => JsonValue::Null,
            Some(value) => JsonValue::Object(value.clone()),
        }
    }
}

/// One committed conversation document copy (upstream
/// `DocumentCommitChange` with `type: "document.copy"`).
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentCopyChange {
    /// New child incarnation record.
    pub record: DocumentRecord,
    /// Owning child conversation.
    pub conversation_id: ConversationId,
    /// Exact persisted source.
    pub source: DocumentCopySource,
}

/// One synchronous post-adoption publication delivered to commit listeners.
#[derive(Debug, Clone, PartialEq)]
pub struct CommitPublication {
    /// Commit sequence assigned by storage.
    pub seq: Seq,
    /// Table writes in staging order followed by document changes.
    pub changes: Vec<CommitChange>,
}

/// Operations an observer applies for one committed change (upstream
/// `observedOperations`, `session.ts:557`).
///
/// A retirement is a root replacement with `null`; a same-version change carries
/// its exact operations; a version change is delivered as a root replacement
/// because the observer holds a differently shaped value.
pub fn observed_operations(
    observed_version: &std::sync::atomic::AtomicU64,
    change: &DocumentChange,
) -> Vec<Op> {
    use std::sync::atomic::Ordering;
    if change.is_retirement() {
        return RETIREMENT_OPERATIONS.to_vec();
    }
    let version = change.version.unwrap_or(0);
    if version == observed_version.load(Ordering::SeqCst) {
        return change.ops.clone();
    }
    observed_version.store(version, Ordering::SeqCst);
    vec![Op::Replace(change.frame_value())]
}

/// Default wall clock (upstream `Date.now`).
pub fn default_now() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}
