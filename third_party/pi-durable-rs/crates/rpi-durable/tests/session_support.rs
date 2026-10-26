#![allow(dead_code)]

//! Shared session-kernel test support: a controlled storage wrapper with
//! observable commits, gated calls, injected failures, and helpers to open a
//! session and create conversations (port of `test/session-support.ts`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rpi_durable::chord::context::{BACKGROUND_CONTEXT, Context};
use rpi_durable::errors::{StorageError, StorageResult};
use rpi_durable::ids::{ConversationId, DocumentId, EntryId, Seq, SubmissionId, TaskId};
use rpi_durable::session::{
    CommitPublication, ConversationOwnership, SessionFuture, SessionImpl, Transaction, commit_fn,
    create_session,
};
use rpi_durable::storage::MemoryStorage;
use rpi_durable::types::{
    ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentPoint, DocumentQuery,
    DocumentRecord, EntryQuery, EntryRecord, Page, Storage, StorageWrite, StoredDocument,
    StoredEntry, SubmissionQuery, SubmissionRecord, TaskQuery, TaskRecord,
};

/// A gate that holds calls until released and reports when the first held call
/// arrives.
pub struct Gate {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl Gate {
    fn new() -> (Self, Arc<Held>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let held = Arc::new(Held {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        (Self { entered, release }, held)
    }

    /// Resolve once the held call has arrived.
    pub async fn entered(&self) {
        self.entered.notified().await;
    }

    /// Release the held call.
    pub fn release(&self) {
        self.release.notify_one();
    }
}

struct Held {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl Held {
    async fn wait(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
}

/// Memory storage with observable commits, held calls, and injected commit
/// failures.
pub struct ControlledStorage {
    inner: MemoryStorage,
    /// Detached batches for value assertions.
    pub commits: Mutex<Vec<Vec<StorageWrite>>>,
    /// Number of ID mint calls.
    pub mint_count: AtomicU64,
    /// Number of document materialization reads.
    pub document_read_count: AtomicU64,
    commit_gate: Mutex<Option<Arc<Held>>>,
    find_gate: Mutex<Option<Arc<Held>>>,
    commit_failure: Mutex<Option<StorageError>>,
    mint_failure: Mutex<Option<StorageError>>,
    find_failure: Mutex<Option<StorageError>>,
    close_failure: Mutex<Option<StorageError>>,
}

impl Default for ControlledStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlledStorage {
    /// Create an empty controlled storage.
    pub fn new() -> Self {
        Self {
            inner: MemoryStorage::new(),
            commits: Mutex::new(Vec::new()),
            mint_count: AtomicU64::new(0),
            document_read_count: AtomicU64::new(0),
            commit_gate: Mutex::new(None),
            find_gate: Mutex::new(None),
            commit_failure: Mutex::new(None),
            mint_failure: Mutex::new(None),
            find_failure: Mutex::new(None),
            close_failure: Mutex::new(None),
        }
    }

    /// Hold the next commit and report when it arrives.
    pub fn hold_commits(&self) -> Gate {
        let (gate, held) = Gate::new();
        *self.commit_gate.lock().expect("gate") = Some(held);
        gate
    }

    /// Hold the next find-document call and report when it arrives.
    pub fn hold_find_document(&self) -> Gate {
        let (gate, held) = Gate::new();
        *self.find_gate.lock().expect("gate") = Some(held);
        gate
    }

    /// Inject one commit failure.
    pub fn fail_next_commit(&self, error: StorageError) {
        *self.commit_failure.lock().expect("failure") = Some(error);
    }
    /// Inject one ID-mint failure.
    pub fn fail_next_mint(&self, error: StorageError) {
        *self.mint_failure.lock().expect("mint failure") = Some(error);
    }

    /// Inject one find-document failure.
    pub fn fail_next_find_document(&self, error: StorageError) {
        *self.find_failure.lock().expect("find failure") = Some(error);
    }

    /// Inject one close failure.
    pub fn fail_next_close(&self, error: StorageError) {
        *self.close_failure.lock().expect("close failure") = Some(error);
    }

    /// Every committed batch, most recent last.
    pub fn last_commit(&self) -> Option<Vec<StorageWrite>> {
        self.commits.lock().expect("commits").last().cloned()
    }

    /// Whether any committed batch satisfies a predicate.
    pub fn any_commit(&self, predicate: impl Fn(&[StorageWrite]) -> bool) -> bool {
        self.commits
            .lock()
            .expect("commits")
            .iter()
            .any(|writes| predicate(writes))
    }
}

#[async_trait]
impl Storage for ControlledStorage {
    async fn commit(&self, writes: &[StorageWrite], context: &Context) -> StorageResult<Seq> {
        self.commits.lock().expect("commits").push(writes.to_vec());
        let held = self.commit_gate.lock().expect("gate").take();
        if let Some(held) = held {
            held.wait().await;
        }
        if let Some(error) = self.commit_failure.lock().expect("failure").take() {
            return Err(error);
        }
        self.inner.commit(writes, context).await
    }

    async fn mint_id(&self) -> StorageResult<u64> {
        self.mint_count.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.mint_failure.lock().expect("mint failure").take() {
            return Err(error);
        }
        self.inner.mint_id().await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &Context,
    ) -> StorageResult<Option<ConversationRecord>> {
        self.inner.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<ConversationRecord>> {
        self.inner
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(&self, id: EntryId, context: &Context) -> StorageResult<Option<StoredEntry>> {
        self.inner.entry(id, context).await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &Context,
    ) -> StorageResult<Option<StoredEntry>> {
        self.inner
            .entry_in_conversation(conversation_id, id, context)
            .await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: &Context,
    ) -> StorageResult<Option<EntryRecord>> {
        self.inner
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, context)
            .await
    }

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<EntryRecord>> {
        self.inner.scan_entries(query, limit, cursor, context).await
    }

    async fn task(&self, id: TaskId, context: &Context) -> StorageResult<Option<TaskRecord>> {
        self.inner.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<TaskRecord>> {
        self.inner.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &Context,
    ) -> StorageResult<Option<SubmissionRecord>> {
        self.inner.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<SubmissionRecord>> {
        self.inner
            .scan_submissions(query, limit, cursor, context)
            .await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &Context,
    ) -> StorageResult<Option<SubmissionRecord>> {
        self.inner
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: &Context,
    ) -> StorageResult<Option<DocumentRecord>> {
        let held = self.find_gate.lock().expect("gate").take();
        if let Some(held) = held {
            held.wait().await;
        }
        if let Some(error) = self.find_failure.lock().expect("find failure").take() {
            return Err(error);
        }
        self.inner.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &Context,
    ) -> StorageResult<Option<StoredDocument>> {
        self.document_read_count.fetch_add(1, Ordering::SeqCst);
        self.inner.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<DocumentRecord>> {
        self.inner
            .scan_documents(query, limit, cursor, context)
            .await
    }

    async fn close(&self, context: &Context) -> StorageResult<()> {
        if let Some(error) = self.close_failure.lock().expect("close failure").take() {
            return Err(error);
        }
        self.inner.close(context).await
    }
}

/// Session kernel plus its controlled storage and every committed publication.
pub struct TestSession {
    /// Controlled storage.
    pub storage: Arc<ControlledStorage>,
    /// Session kernel.
    pub session: Arc<SessionImpl>,
    /// Every commit publication, in delivery order.
    pub publications: Arc<Mutex<Vec<CommitPublication>>>,
}

/// Open a session over a controlled storage with a publication recorder.
pub fn open_test_session() -> TestSession {
    let storage = Arc::new(ControlledStorage::new());
    let session = create_session(Arc::clone(&storage) as Arc<dyn Storage>, None);
    let publications = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&publications);
    session.subscribe_commits(Arc::new(move |publication, _context| {
        recorder
            .lock()
            .expect("publications")
            .push(publication.clone());
    }));
    TestSession {
        storage,
        session,
        publications,
    }
}

/// Create one ownerless conversation and return its ID.
pub async fn create_conversation(session: &Arc<SessionImpl>) -> ConversationId {
    session
        .commit(
            commit_fn(|tx: &mut Transaction| {
                Box::pin(async move {
                    Ok(tx
                        .create_conversation(ConversationOwnership::Ownerless)
                        .await?
                        .id)
                })
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("create conversation")
}

/// Commit a callback with no scope and expect success.
pub async fn commit<T, F>(session: &Arc<SessionImpl>, change: F) -> T
where
    T: Send + 'static,
    F: for<'a> Fn(&'a mut Transaction) -> SessionFuture<'a, T> + Send + Sync + 'static,
{
    session
        .commit(commit_fn(change), &BACKGROUND_CONTEXT)
        .await
        .expect("commit")
}

/// Commit a callback with no scope and return the result.
pub async fn try_commit<T, F>(
    session: &Arc<SessionImpl>,
    change: F,
) -> Result<T, rpi_durable::session::SessionError>
where
    T: Send + 'static,
    F: for<'a> Fn(&'a mut Transaction) -> SessionFuture<'a, T> + Send + Sync + 'static,
{
    session.commit(commit_fn(change), &BACKGROUND_CONTEXT).await
}

/// Await pending asynchronous watch deliveries.
pub async fn flush() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
}

/// Extract document changes from a publication.
pub fn document_changes(
    publication: &CommitPublication,
) -> Vec<rpi_durable::session::DocumentChange> {
    publication
        .changes
        .iter()
        .filter_map(|change| match change {
            rpi_durable::session::CommitChange::Document(document) => Some(document.clone()),
            _ => None,
        })
        .collect()
}

/// Extract document copy changes from a publication.
pub fn document_copy_changes(
    publication: &CommitPublication,
) -> Vec<rpi_durable::session::DocumentCopyChange> {
    publication
        .changes
        .iter()
        .filter_map(|change| match change {
            rpi_durable::session::CommitChange::DocumentCopy(copy) => Some(copy.clone()),
            _ => None,
        })
        .collect()
}
