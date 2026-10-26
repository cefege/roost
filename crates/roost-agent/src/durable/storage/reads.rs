//! Indexed Durable reads over the confirmed in-memory storage projection.
//! Mutations delegate to the parent's fenced SQL settlement line; closed or
//! cancelled queries reject before consulting the upstream query implementation.

use async_trait::async_trait;
use rpi_durable::{
    Context, ConversationId, ConversationQuery, ConversationRecord, Cursor, DocumentAddress,
    DocumentId, DocumentPoint, DocumentQuery, DocumentRecord, EntryId, EntryQuery, EntryRecord,
    Page, Seq, Storage, StorageResult, StorageWrite, StoredDocument, StoredEntry, SubmissionId,
    SubmissionQuery, SubmissionRecord, TaskId, TaskQuery, TaskRecord,
};

use super::JournalStorage;

#[async_trait]
impl Storage for JournalStorage {
    async fn commit(&self, writes: &[StorageWrite], context: &Context) -> StorageResult<Seq> {
        self.commit_batch(writes, context).await
    }

    async fn mint_id(&self) -> StorageResult<u64> {
        self.reserve_id().await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &Context,
    ) -> StorageResult<Option<ConversationRecord>> {
        self.check(context)?;
        self.memory.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<ConversationRecord>> {
        self.check(context)?;
        self.memory
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(&self, id: EntryId, context: &Context) -> StorageResult<Option<StoredEntry>> {
        self.check(context)?;
        self.memory.entry(id, context).await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &Context,
    ) -> StorageResult<Option<StoredEntry>> {
        self.check(context)?;
        self.memory
            .entry_in_conversation(conversation_id, id, context)
            .await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: &Context,
    ) -> StorageResult<Option<EntryRecord>> {
        self.check(context)?;
        self.memory
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
        self.check(context)?;
        self.memory
            .scan_entries(query, limit, cursor, context)
            .await
    }

    async fn task(&self, id: TaskId, context: &Context) -> StorageResult<Option<TaskRecord>> {
        self.check(context)?;
        self.memory.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<TaskRecord>> {
        self.check(context)?;
        self.memory.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &Context,
    ) -> StorageResult<Option<SubmissionRecord>> {
        self.check(context)?;
        self.memory.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<SubmissionRecord>> {
        self.check(context)?;
        self.memory
            .scan_submissions(query, limit, cursor, context)
            .await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &Context,
    ) -> StorageResult<Option<SubmissionRecord>> {
        self.check(context)?;
        self.memory
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: &Context,
    ) -> StorageResult<Option<DocumentRecord>> {
        self.check(context)?;
        self.memory.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &Context,
    ) -> StorageResult<Option<StoredDocument>> {
        self.check(context)?;
        self.memory.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<DocumentRecord>> {
        self.check(context)?;
        self.memory
            .scan_documents(query, limit, cursor, context)
            .await
    }

    async fn close(&self, context: &Context) -> StorageResult<()> {
        self.close_storage(context).await
    }
}
