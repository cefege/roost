#![allow(dead_code)]

//! Shared test support: temporary directories and a reopening `Storage` wrapper
//! used to port the upstream `across reopen` conformance legs.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rpi_durable::chord::context::Context;
use rpi_durable::errors::StorageResult;
use rpi_durable::ids::{ConversationId, DocumentId, EntryId, Seq, SubmissionId, TaskId};
use rpi_durable::types::{
    ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentPoint, DocumentQuery,
    DocumentRecord, EntryQuery, EntryRecord, Page, Storage, StorageWrite, StoredDocument,
    StoredEntry, SubmissionQuery, SubmissionRecord, TaskQuery, TaskRecord,
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

fn registry() -> &'static Mutex<Vec<PathBuf>> {
    static REGISTRY: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// A temporary directory registered for end-of-test cleanup.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a fresh directory under the system temp root.
    pub fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        registry().lock().expect("registry").push(path.clone());
        Self { path }
    }

    /// The directory path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Path as a string.
    pub fn path_str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

/// Remove registered temporary directories whose name starts with `prefix`
/// (scoped so concurrently running tests do not delete each other's roots).
pub fn cleanup_temp_dirs(prefix: &str) {
    let expected = format!("{prefix}-{}", std::process::id());
    if let Ok(mut dirs) = registry().lock() {
        dirs.retain(|path| {
            let matches = path
                .file_name()
                .map(|name| name.to_string_lossy().starts_with(&expected))
                .unwrap_or(false);
            if matches {
                let _ = std::fs::remove_dir_all(path);
            }
            !matches
        });
    }
}

/// Boxed reopen future.
pub type ReopenFuture = Pin<Box<dyn Future<Output = StorageResult<Box<dyn Storage>>> + Send>>;

/// Wraps a storage and closes/reopens it after each successful commit, matching
/// the upstream `ReopeningStorage` test doubles.
pub struct ReopeningStorage {
    inner: tokio::sync::Mutex<Box<dyn Storage>>,
    reopen: Box<dyn Fn() -> ReopenFuture + Send + Sync>,
    closed: std::sync::atomic::AtomicBool,
}

impl ReopeningStorage {
    /// Wrap `inner` with a reopen factory.
    pub fn new(
        inner: Box<dyn Storage>,
        reopen: impl Fn() -> ReopenFuture + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(inner),
            reopen: Box::new(reopen),
            closed: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl Storage for ReopeningStorage {
    async fn commit(&self, writes: &[StorageWrite], context: &Context) -> StorageResult<Seq> {
        let mut guard = self.inner.lock().await;
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return guard.commit(writes, context).await;
        }
        let result = guard.commit(writes, context).await;
        // Upstream reopens in `finally`, so rejected commits are covered too.
        guard.close(context).await?;
        let reopened = (self.reopen)().await?;
        *guard = reopened;
        result
    }

    async fn mint_id(&self) -> StorageResult<u64> {
        self.inner.lock().await.mint_id().await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &Context,
    ) -> StorageResult<Option<ConversationRecord>> {
        self.inner.lock().await.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<ConversationRecord>> {
        self.inner
            .lock()
            .await
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(&self, id: EntryId, context: &Context) -> StorageResult<Option<StoredEntry>> {
        self.inner.lock().await.entry(id, context).await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &Context,
    ) -> StorageResult<Option<StoredEntry>> {
        self.inner
            .lock()
            .await
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
            .lock()
            .await
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
        self.inner
            .lock()
            .await
            .scan_entries(query, limit, cursor, context)
            .await
    }

    async fn task(&self, id: TaskId, context: &Context) -> StorageResult<Option<TaskRecord>> {
        self.inner.lock().await.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<TaskRecord>> {
        self.inner
            .lock()
            .await
            .scan_tasks(query, limit, cursor, context)
            .await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &Context,
    ) -> StorageResult<Option<SubmissionRecord>> {
        self.inner.lock().await.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<SubmissionRecord>> {
        self.inner
            .lock()
            .await
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
            .lock()
            .await
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: &Context,
    ) -> StorageResult<Option<DocumentRecord>> {
        self.inner
            .lock()
            .await
            .find_document(address, at, context)
            .await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &Context,
    ) -> StorageResult<Option<StoredDocument>> {
        self.inner.lock().await.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<DocumentRecord>> {
        self.inner
            .lock()
            .await
            .scan_documents(query, limit, cursor, context)
            .await
    }

    async fn close(&self, context: &Context) -> StorageResult<()> {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        self.inner.lock().await.close(context).await
    }
}

/// Arc alias used by conformance providers.
pub type SharedStorage = Arc<dyn Storage>;
