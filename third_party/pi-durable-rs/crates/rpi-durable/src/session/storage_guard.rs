//! Session-scoped guard for the raw durable backend.
//!
//! Every backend operation checks the session failure before admission and
//! checks it again after settlement so calls already underway observe the first
//! fatal cause. The close operation alone bypasses admission to release resources.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::chord::context::Context;
use crate::errors::{StorageError, StorageResult};
use crate::ids::{ConversationId, DocumentId, EntryId, Seq, SubmissionId, TaskId};
use crate::types::{
    ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentPoint, DocumentQuery,
    DocumentRecord, EntryQuery, EntryRecord, Page, Storage, StorageWrite, StoredDocument,
    StoredEntry, SubmissionQuery, SubmissionRecord, TaskQuery, TaskRecord,
};

/// Storage facade that seals its session at the first unexpected failure.
pub(super) struct GuardedStorage {
    inner: Arc<dyn Storage>,
    failure: Arc<Mutex<Option<String>>>,
    on_failure: Arc<dyn Fn(String) + Send + Sync>,
}

impl GuardedStorage {
    pub(super) fn new(
        inner: Arc<dyn Storage>,
        failure: Arc<Mutex<Option<String>>>,
        on_failure: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner,
            failure,
            on_failure,
        })
    }

    fn failed(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|failure| failure.clone())
    }

    fn admit(&self) -> StorageResult<()> {
        match self.failed() {
            Some(cause) => Err(StorageError::SessionFailed(cause)),
            None => Ok(()),
        }
    }

    fn settle<T>(
        &self,
        result: StorageResult<T>,
        context: Option<&Context>,
        always_fatal: bool,
    ) -> StorageResult<T> {
        match result {
            Ok(value) => match self.failed() {
                Some(cause) => Err(StorageError::SessionFailed(cause)),
                None => Ok(value),
            },
            Err(error) => {
                if let Some(cause) = self.failed() {
                    return Err(StorageError::SessionFailed(cause));
                }
                let aborted = matches!(
                    &error,
                    StorageError::File(file)
                        if file.code == crate::errors::FileErrorCode::Aborted
                ) || context
                    .and_then(Context::abort_signal)
                    .is_some_and(|signal| signal.is_cancelled());
                if always_fatal || (!aborted && !error.is_request_error()) {
                    let cause = error.to_string();
                    if let Ok(mut failure) = self.failure.lock() {
                        if let Some(first) = failure.as_ref() {
                            return Err(StorageError::SessionFailed(first.clone()));
                        }
                        *failure = Some(cause.clone());
                    }
                    (self.on_failure)(cause);
                }
                Err(error)
            }
        }
    }
}

#[async_trait]
impl Storage for GuardedStorage {
    async fn commit(&self, writes: &[StorageWrite], context: &Context) -> StorageResult<Seq> {
        self.admit()?;
        let result = self.inner.commit(writes, context).await;
        self.settle(result, Some(context), true)
    }

    async fn mint_id(&self) -> StorageResult<u64> {
        self.admit()?;
        let result = self.inner.mint_id().await;
        self.settle(result, None, true)
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &Context,
    ) -> StorageResult<Option<ConversationRecord>> {
        self.admit()?;
        let result = self.inner.conversation(id, context).await;
        self.settle(result, Some(context), false)
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<ConversationRecord>> {
        self.admit()?;
        let result = self
            .inner
            .scan_conversations(query, limit, cursor, context)
            .await;
        self.settle(result, Some(context), false)
    }

    async fn entry(&self, id: EntryId, context: &Context) -> StorageResult<Option<StoredEntry>> {
        self.admit()?;
        let result = self.inner.entry(id, context).await;
        self.settle(result, Some(context), false)
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &Context,
    ) -> StorageResult<Option<StoredEntry>> {
        self.admit()?;
        let result = self
            .inner
            .entry_in_conversation(conversation_id, id, context)
            .await;
        self.settle(result, Some(context), false)
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: &Context,
    ) -> StorageResult<Option<EntryRecord>> {
        self.admit()?;
        let result = self
            .inner
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, context)
            .await;
        self.settle(result, Some(context), false)
    }

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<EntryRecord>> {
        self.admit()?;
        let result = self.inner.scan_entries(query, limit, cursor, context).await;
        self.settle(result, Some(context), false)
    }

    async fn task(&self, id: TaskId, context: &Context) -> StorageResult<Option<TaskRecord>> {
        self.admit()?;
        let result = self.inner.task(id, context).await;
        self.settle(result, Some(context), false)
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<TaskRecord>> {
        self.admit()?;
        let result = self.inner.scan_tasks(query, limit, cursor, context).await;
        self.settle(result, Some(context), false)
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &Context,
    ) -> StorageResult<Option<SubmissionRecord>> {
        self.admit()?;
        let result = self.inner.submission(id, context).await;
        self.settle(result, Some(context), false)
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<SubmissionRecord>> {
        self.admit()?;
        let result = self
            .inner
            .scan_submissions(query, limit, cursor, context)
            .await;
        self.settle(result, Some(context), false)
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &Context,
    ) -> StorageResult<Option<SubmissionRecord>> {
        self.admit()?;
        let result = self
            .inner
            .submission_by_request(conversation_id, request_id, context)
            .await;
        self.settle(result, Some(context), false)
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: &Context,
    ) -> StorageResult<Option<DocumentRecord>> {
        self.admit()?;
        let result = self.inner.find_document(address, at, context).await;
        self.settle(result, Some(context), false)
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &Context,
    ) -> StorageResult<Option<StoredDocument>> {
        self.admit()?;
        let result = self.inner.document(id, at, context).await;
        self.settle(result, Some(context), false)
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<DocumentRecord>> {
        self.admit()?;
        let result = self
            .inner
            .scan_documents(query, limit, cursor, context)
            .await;
        self.settle(result, Some(context), false)
    }

    async fn close(&self, context: &Context) -> StorageResult<()> {
        match self.inner.close(context).await {
            Ok(()) => Ok(()),
            Err(error) => self.settle(Err(error), None, true),
        }
    }
}
