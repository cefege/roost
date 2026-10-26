//! Session kernel: one mutation line, the loaded document tracker cache, and
//! committed publication (port of `packages/durable/src/session/session.ts`
//! @ `abe508e1b`).
//!
//! Only committed state is observable. Every commit callback, storage
//! settlement, adoption and publication runs while the single mutation line is
//! held; listeners run after adoption but before the line releases, and must not
//! re-enter.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::transaction::doc_target;
use crate::chord::context::{AbortSignal, Context, without_abort_signal};
use crate::chord::json::{JsonObject, JsonValue};
use crate::chord::tracker::track;
use crate::documents::{
    DocScope, DocTarget, DocToken, check_record_scope_record, check_record_version_record,
    materialize_document, resolve_address,
};
use crate::errors::StorageError;
use crate::ids::{ConversationId, DocumentId, EntryId, Seq};
use crate::session::observation::{
    CommittedStateSource, CommittedWatch, WatchListener, attach_state,
};
use crate::session::transaction::{LoadedDocument, Transaction, TransactionScope};
use crate::session::{
    CommitChange, CommitPublication, DocOwner, SessionError, SessionFuture, SessionResult,
    Subscription, observed_operations,
};
use crate::types::{ConversationRecord, DocumentAddress, DocumentRecord, DocumentScope, Storage};
/// Construction policy for a session kernel.
#[derive(Clone)]
pub struct SessionOptions {
    /// Refuse every transaction that prepares storage writes.
    pub read_only: bool,
    /// Default deferred persistence window in milliseconds; infinity waits for dependency or close.
    pub persist_within_ms: f64,
    /// Arms persistence deadlines; absent uses the Tokio timer.
    pub set_timer: Option<crate::harness::scheduler::SetTimer>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            read_only: false,
            persist_within_ms: 1_000.0,
            set_timer: None,
        }
    }
}

/// Persistence choice for one commit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CommitPersistence {
    /// Store as part of this call.
    Immediate,
    /// Adopt eligible document-only writes in memory until a deadline or a dependent durable write.
    Deferred { within_ms: Option<f64> },
}

/// Per-commit persistence options.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CommitOptions {
    /// Persistence policy; absent means immediate.
    pub persistence: Option<CommitPersistence>,
}

/// Wall clock used for task lifecycle times (upstream `Date.now`).
pub type NowFn = Arc<dyn Fn() -> f64 + Send + Sync>;

/// A synchronous post-adoption commit listener. It must not throw, block, or
/// call session operations (upstream `subscribeCommits`).
pub type CommitListener = Arc<dyn Fn(&CommitPublication, &Context) + Send + Sync>;
/// A synchronous observer of adopted document changes, including memory-only deferred revisions.
pub type MemoryCommitListener = Arc<dyn Fn(&[CommitChange], &Context) + Send + Sync>;

/// A synchronous close listener, called once when close begins (upstream
/// `subscribeClose`).
pub type CloseListener = Arc<dyn Fn() + Send + Sync>;

/// One commit callback: receives the concrete transaction and returns a value.
pub type CommitFn<T> =
    Arc<dyn for<'a> Fn(&'a mut Transaction) -> SessionFuture<'a, T> + Send + Sync>;

/// A hook staged into every transaction that creates or forks a conversation.
pub type ConversationCreatedHook = Arc<
    dyn for<'a, 'r> Fn(&'a mut Transaction, &'r ConversationRecord) -> SessionFuture<'a, ()>
        + Send
        + Sync,
>;

/// Wrap a closure as a [`CommitFn`].
pub fn commit_fn<T, F>(function: F) -> CommitFn<T>
where
    F: for<'a> Fn(&'a mut Transaction) -> SessionFuture<'a, T> + Send + Sync + 'static,
{
    Arc::new(function)
}

static NEXT_LISTENER_ID: AtomicU64 = AtomicU64::new(1);

fn error_future<'a, T>(error: SessionError) -> SessionFuture<'a, T> {
    Box::pin(async move { Err(error) })
}

/// Reset an atomic flag when dropped unless disarmed. Close paths use this so a
/// cancelled or failed await leaves the resource retryable.
struct ResetOnDrop<'a> {
    flag: &'a AtomicBool,
    armed: bool,
}

impl<'a> ResetOnDrop<'a> {
    fn new(flag: &'a AtomicBool) -> Self {
        Self { flag, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ResetOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.flag.store(false, Ordering::SeqCst);
        }
    }
}

/// A conversation document's current incarnation and value, resolved on the line
/// (upstream `conversation_document_on_line`).
#[derive(Debug, Clone)]
pub struct ConversationDocumentView {
    /// Current incarnation record.
    pub record: DocumentRecord,
    /// Definition version of `value`.
    pub version: u64,
    /// Committed value.
    pub value: JsonObject,
}

#[derive(Clone)]
struct Hold {
    loaded: Arc<LoadedDocument>,
    ops: Vec<crate::chord::delta::Op>,
    base: bool,
    deadline: f64,
    conversation_id: Option<ConversationId>,
    version: u64,
}

/// Session kernel (upstream `SessionImpl`). Always shared as `Arc<SessionImpl>`.
pub struct SessionImpl {
    self_weak: std::sync::Weak<SessionImpl>,
    storage: Arc<dyn Storage>,
    now: NowFn,
    options: SessionOptions,
    documents: Mutex<HashMap<String, Arc<LoadedDocument>>>,
    holds: Mutex<HashMap<DocumentId, Hold>>,
    hold_timer: Mutex<Option<(f64, crate::harness::scheduler::TimerDisarm)>>,
    commit_listeners: Arc<Mutex<Vec<(u64, CommitListener)>>>,
    memory_listeners: Arc<Mutex<Vec<(u64, MemoryCommitListener)>>>,
    close_listeners: Arc<Mutex<Vec<(u64, CloseListener)>>>,
    conversation_created_hook: Mutex<Option<ConversationCreatedHook>>,
    line: tokio::sync::Mutex<()>,
    closing: AtomicBool,
    fatal_cause: Arc<Mutex<Option<String>>>,
    cleanup_done: AtomicBool,
    close_error: Mutex<Option<String>>,
}

/// Open a session kernel over one storage backend (upstream `createSession`).
pub fn create_session(storage: Arc<dyn Storage>, now: Option<NowFn>) -> Arc<SessionImpl> {
    SessionImpl::new(storage, now)
}

impl SessionImpl {
    /// Build a session over `storage` with default lifecycle options.
    pub fn new(storage: Arc<dyn Storage>, now: Option<NowFn>) -> Arc<Self> {
        Self::new_with_options(storage, now, SessionOptions::default())
    }

    /// Build a session with explicit read-only and deferred-persistence policy.
    pub fn new_with_options(
        storage: Arc<dyn Storage>,
        now: Option<NowFn>,
        options: SessionOptions,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak: &std::sync::Weak<Self>| {
            let fatal_cause = Arc::new(Mutex::new(None));
            let failure_session = weak.clone();
            let on_failure = Arc::new(move |cause| {
                if let Some(session) = failure_session.upgrade() {
                    session.fail_session(cause);
                }
            });
            let guarded = super::storage_guard::GuardedStorage::new(
                storage,
                Arc::clone(&fatal_cause),
                on_failure,
            );
            let storage: Arc<dyn Storage> = guarded;
            Self {
                self_weak: weak.clone(),
                storage,
                now: now.unwrap_or_else(|| Arc::new(crate::session::default_now)),
                options,
                documents: Mutex::new(HashMap::new()),
                holds: Mutex::new(HashMap::new()),
                hold_timer: Mutex::new(None),
                commit_listeners: Arc::new(Mutex::new(Vec::new())),
                memory_listeners: Arc::new(Mutex::new(Vec::new())),
                close_listeners: Arc::new(Mutex::new(Vec::new())),
                conversation_created_hook: Mutex::new(None),
                line: tokio::sync::Mutex::new(()),
                closing: AtomicBool::new(false),
                fatal_cause,
                cleanup_done: AtomicBool::new(false),
                close_error: Mutex::new(None),
            }
        })
    }

    /// The storage backend.
    pub fn storage(&self) -> &Arc<dyn Storage> {
        &self.storage
    }
    pub(crate) fn read_only(&self) -> bool {
        self.options.read_only
    }

    /// Current wall clock in milliseconds.
    pub fn now(&self) -> f64 {
        (self.now)()
    }

    /// Install a conversation-created hook (base sessions stage nothing).
    pub fn set_conversation_created_hook(&self, hook: ConversationCreatedHook) {
        if let Ok(mut slot) = self.conversation_created_hook.lock() {
            *slot = Some(hook);
        }
    }

    pub(crate) fn cached(&self, address_id: &str) -> Option<Arc<LoadedDocument>> {
        self.documents
            .lock()
            .ok()
            .and_then(|documents| documents.get(address_id).cloned())
    }

    pub(crate) fn install(&self, document: Arc<LoadedDocument>) {
        if let Ok(mut documents) = self.documents.lock() {
            documents.insert(document.address_id.clone(), document);
        }
    }

    pub(crate) fn evict(&self, address_id: &str, record_id: DocumentId) {
        if let Ok(mut documents) = self.documents.lock()
            && documents
                .get(address_id)
                .map(|document| document.record.id == record_id)
                .unwrap_or(false)
        {
            documents.remove(address_id);
        }
    }

    pub(crate) async fn load_document(
        &self,
        definition: &Arc<crate::documents::DocDefinition>,
        address_id: &str,
        address: &DocumentAddress,
        context: &Context,
    ) -> SessionResult<Option<Arc<LoadedDocument>>> {
        let cached = self.cached(address_id);
        if let Some(cached) = &cached
            && cached.value_version == definition.version
        {
            return Ok(Some(Arc::clone(cached)));
        }
        if cached.is_some()
            && let Ok(mut documents) = self.documents.lock()
        {
            documents.remove(address_id);
        }
        let record = match self
            .storage
            .find_document(
                address.clone(),
                crate::types::DocumentPoint::CURRENT,
                context,
            )
            .await
        {
            Ok(record) => record,
            Err(error) => {
                self.fail_unexpected_storage(&error, Some(context));
                return Err(error.into());
            }
        };
        let Some(record) = record else {
            return Ok(None);
        };
        let stored = match self
            .storage
            .document(record.id, crate::types::DocumentPoint::CURRENT, context)
            .await
        {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                return Err(SessionError::message(format!(
                    "Current document {} ({}) cannot be read",
                    record.id, record.kind
                )));
            }
            Err(error) => {
                self.fail_unexpected_storage(&error, Some(context));
                return Err(error.into());
            }
        };
        let value = materialize_document(definition, &stored)?;
        let loaded = Arc::new(LoadedDocument::new(
            address_id.to_string(),
            stored.record,
            stored.version,
            definition.version,
            stored.deltas_since_base,
            Arc::new(track(JsonValue::Object(value))),
        ));
        self.install(Arc::clone(&loaded));
        Ok(Some(loaded))
    }

    pub(crate) fn conversation_created<'a>(
        self: &'a Arc<Self>,
        tx: &'a mut Transaction,
        record: &'a ConversationRecord,
    ) -> SessionFuture<'a, ()> {
        let hook = self
            .conversation_created_hook
            .lock()
            .ok()
            .and_then(|slot| slot.clone());
        match hook {
            None => Box::pin(async { Ok(()) }),
            Some(hook) => hook(tx, record),
        }
    }

    // ─── Commit line ────────────────────────────────────────────────────────

    /// Commit one callback with an explicit default scope.
    pub fn commit_with<T: Send + 'static>(
        self: &Arc<Self>,
        change: CommitFn<T>,
        context: &Context,
        scope: Option<TransactionScope>,
    ) -> SessionFuture<'_, T> {
        self.commit_with_options(change, context, scope, CommitOptions::default())
    }

    /// Commit one callback with an explicit persistence policy.
    pub fn commit_with_options<T: Send + 'static>(
        self: &Arc<Self>,
        change: CommitFn<T>,
        context: &Context,
        scope: Option<TransactionScope>,
        options: CommitOptions,
    ) -> SessionFuture<'_, T> {
        if let Err(error) = self.assert_usable() {
            return error_future(error);
        }
        let within_ms = match options.persistence {
            None | Some(CommitPersistence::Immediate) => None,
            Some(CommitPersistence::Deferred { within_ms }) => {
                let within_ms = within_ms.unwrap_or(self.options.persist_within_ms);
                if !(within_ms >= 0.0) {
                    return error_future(SessionError::message(
                        "CommitPersistence.within_ms must be non-negative",
                    ));
                }
                Some(within_ms)
            }
        };
        let this = Arc::clone(self);
        let context = context.clone();
        Box::pin(async move { this.run_commit(change, context, scope, within_ms).await })
    }

    /// Commit one callback (upstream `commit`).
    pub fn commit<T: Send + 'static>(
        self: &Arc<Self>,
        change: CommitFn<T>,
        context: &Context,
    ) -> SessionFuture<'_, T> {
        self.commit_with(change, context, None)
    }

    async fn run_commit<T: Send>(
        self: Arc<Self>,
        change: CommitFn<T>,
        context: Context,
        scope: Option<TransactionScope>,
        within_ms: Option<f64>,
    ) -> SessionResult<T> {
        let _guard = self.line.lock().await;
        self.assert_healthy()?;
        if context
            .abort_signal()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(SessionError::Aborted);
        }
        let mut tx = Transaction::new(
            Arc::clone(&self),
            context.clone(),
            scope.unwrap_or_default(),
        );
        let result = match (change)(&mut tx).await {
            Ok(value) => value,
            Err(error) => {
                tx.settle_failure().await;
                if let SessionError::Storage(storage_error) = &error
                    && !storage_error.is_request_error()
                    && !context
                        .abort_signal()
                        .is_some_and(|signal| signal.is_cancelled())
                {
                    self.fail_session(storage_error.to_string());
                }
                return Err(error);
            }
        };
        let writes = match tx.settle_success().await {
            Ok(writes) => writes,
            Err(error) => {
                if let SessionError::Storage(storage_error) = &error {
                    self.fail_unexpected_storage(storage_error, Some(&context));
                }
                return Err(error);
            }
        };
        if writes.is_empty() {
            tx.discard();
            return Ok(result);
        }
        if self.options.read_only {
            tx.discard();
            return Err(SessionError::ReadOnlySession("a commit with writes"));
        }
        if let Some(within_ms) = within_ms {
            if let Some(held) = tx.holdable() {
                let documents = match tx.adopt_held() {
                    Ok(documents) => documents,
                    Err(error) => {
                        self.fail_session(error.to_string());
                        return Err(error);
                    }
                };
                if let Err(error) = self.add_holds(held, within_ms) {
                    return Err(error);
                }
                self.publish_memory(&documents, &context);
                self.arm_hold_timer();
                if within_ms == 0.0 {
                    self.store_holds(&without_abort_signal(&context)).await?;
                }
                return Ok(result);
            }
        }
        let has_holds = !self
            .holds
            .lock()
            .map_err(|_| SessionError::message("Session held-document lock poisoned"))?
            .is_empty();
        if has_holds {
            if let Err(error) = self.store_holds(&without_abort_signal(&context)).await {
                tx.discard();
                return Err(error);
            }
        }
        let seq = match self
            .storage
            .commit(&writes, &without_abort_signal(&context))
            .await
        {
            Ok(seq) => seq,
            Err(error) => {
                tx.discard();
                self.fail_session(error.to_string());
                return Err(SessionError::Storage(error));
            }
        };
        let documents = match tx.adopt(seq) {
            Ok(documents) => documents,
            Err(error) => {
                self.fail_session(error.to_string());
                return Err(error);
            }
        };
        self.publish(seq, &writes, &documents, &context, true);
        Ok(result)
    }
    fn add_holds(
        &self,
        candidates: Vec<crate::session::transaction::HoldableDocument>,
        within_ms: f64,
    ) -> SessionResult<()> {
        let deadline = self.now() + within_ms;
        let mut holds = self
            .holds
            .lock()
            .map_err(|_| SessionError::message("Session held-document lock poisoned"))?;
        for candidate in candidates {
            let hold = holds
                .entry(candidate.loaded.record.id)
                .or_insert_with(|| Hold {
                    loaded: Arc::clone(&candidate.loaded),
                    ops: Vec::new(),
                    base: false,
                    deadline,
                    conversation_id: candidate.conversation_id,
                    version: candidate.version,
                });
            hold.ops.extend(candidate.ops);
            hold.base |= candidate.base;
            hold.deadline = hold.deadline.min(deadline);
        }
        Ok(())
    }

    fn arm_hold_timer(self: &Arc<Self>) {
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        let earliest = self
            .holds
            .lock()
            .map(|holds| {
                holds
                    .values()
                    .map(|hold| hold.deadline)
                    .fold(f64::INFINITY, f64::min)
            })
            .unwrap_or(f64::INFINITY);
        let Ok(mut timer) = self.hold_timer.lock() else {
            self.fail_session("Session hold-timer lock poisoned".to_string());
            return;
        };
        if timer.as_ref().is_some_and(|(at, _)| *at == earliest) {
            return;
        }
        if let Some((_, disarm)) = timer.take() {
            disarm();
        }
        if !earliest.is_finite() {
            return;
        }
        let weak = Arc::downgrade(self);
        let callback: crate::harness::scheduler::TimerCallback = Arc::new(move || {
            if let Some(session) = weak.upgrade() {
                tokio::spawn(async move {
                    let _guard = session.line.lock().await;
                    if session.closing.load(Ordering::SeqCst) || session.assert_healthy().is_err() {
                        return;
                    }
                    if let Err(error) = session
                        .store_holds(&crate::chord::context::BACKGROUND_CONTEXT)
                        .await
                    {
                        session.fail_session(error.to_string());
                    }
                });
            }
        });
        let delay = (earliest - self.now()).max(0.0);
        let disarm = self
            .options
            .set_timer
            .as_ref()
            .and_then(|set_timer| set_timer(Arc::clone(&callback), delay, false))
            .unwrap_or_else(|| {
                let cancelled = Arc::new(AtomicBool::new(false));
                let timer_cancelled = Arc::clone(&cancelled);
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs_f64(delay / 1_000.0)).await;
                    if !timer_cancelled.load(Ordering::SeqCst) {
                        callback();
                    }
                });
                Arc::new(move || cancelled.store(true, Ordering::SeqCst))
            });
        *timer = Some((earliest, disarm));
    }

    async fn store_holds(&self, context: &Context) -> SessionResult<()> {
        let holds = {
            let guard = self
                .holds
                .lock()
                .map_err(|_| SessionError::message("Session held-document lock poisoned"))?;
            guard.values().cloned().collect::<Vec<_>>()
        };
        if holds.is_empty() {
            return Ok(());
        }
        let mut writes = Vec::with_capacity(holds.len());
        let mut changes = Vec::with_capacity(holds.len());
        for hold in &holds {
            let value = hold
                .loaded
                .tracker
                .value()
                .as_object()
                .cloned()
                .unwrap_or_default();
            let content = if hold.base {
                crate::types::DocumentContent::Base {
                    version: hold.version,
                    value: value.clone(),
                }
            } else {
                crate::types::DocumentContent::Delta {
                    version: hold.version,
                    ops: hold.ops.clone(),
                }
            };
            writes.push(crate::types::StorageWrite::DocumentChange {
                id: hold.loaded.record.id,
                content,
            });
            changes.push(CommitChange::Document(crate::session::DocumentChange {
                record: hold.loaded.record.clone(),
                conversation_id: hold.conversation_id,
                version: Some(hold.version),
                value: Some(value),
                ops: hold.ops.clone(),
            }));
        }
        let seq = match self
            .storage
            .commit(&writes, &without_abort_signal(context))
            .await
        {
            Ok(seq) => seq,
            Err(error) => {
                self.fail_session(error.to_string());
                return Err(SessionError::Storage(error));
            }
        };
        for hold in &holds {
            if hold.base {
                hold.loaded.set_deltas_since_base(0);
            } else {
                hold.loaded
                    .set_deltas_since_base(hold.loaded.deltas_since_base().saturating_add(1));
            }
        }
        {
            let mut pending = self
                .holds
                .lock()
                .map_err(|_| SessionError::message("Session held-document lock poisoned"))?;
            for hold in &holds {
                pending.remove(&hold.loaded.record.id);
            }
        }
        if let Ok(mut timer) = self.hold_timer.lock() {
            if let Some((_, disarm)) = timer.take() {
                disarm();
            }
        }
        self.publish(seq, &writes, &changes, context, false);
        Ok(())
    }

    fn publish(
        &self,
        seq: Seq,
        writes: &[crate::types::StorageWrite],
        documents: &[CommitChange],
        context: &Context,
        notify_memory: bool,
    ) {
        let listeners: Vec<CommitListener> = match self.commit_listeners.lock() {
            Ok(listeners) => listeners
                .iter()
                .map(|(_, listener)| Arc::clone(listener))
                .collect(),
            Err(_) => return,
        };
        let mut changes = Vec::new();
        for write in writes {
            match write {
                crate::types::StorageWrite::Conversation { .. }
                | crate::types::StorageWrite::Entry { .. }
                | crate::types::StorageWrite::Task { .. }
                | crate::types::StorageWrite::Submission { .. } => {
                    changes.push(CommitChange::Table(write.clone()));
                }
                _ => {}
            }
        }
        changes.extend(documents.iter().cloned());
        let publication = CommitPublication { seq, changes };
        for listener in listeners {
            listener(&publication, context);
        }
        if notify_memory {
            self.publish_memory(documents, context);
        }
    }

    fn publish_memory(&self, documents: &[CommitChange], context: &Context) {
        let listeners: Vec<MemoryCommitListener> = match self.memory_listeners.lock() {
            Ok(listeners) => listeners
                .iter()
                .map(|(_, listener)| Arc::clone(listener))
                .collect(),
            Err(_) => return,
        };
        for listener in listeners {
            listener(documents, context);
        }
    }

    /// Run a read-only job on the mutation line (upstream `readOnLine`).
    pub fn read_on_line<T, F>(self: &Arc<Self>, job: F) -> SessionFuture<'_, T>
    where
        T: Send + 'static,
        F: FnOnce() -> SessionFuture<'static, T> + Send + 'static,
    {
        if let Err(error) = self.assert_usable() {
            return error_future(error);
        }
        let this = Arc::clone(self);
        Box::pin(async move {
            let _guard = this.line.lock().await;
            this.assert_healthy()?;
            job().await
        })
    }

    // ─── Reads ──────────────────────────────────────────────────────────────

    /// Current committed value of one document, or `None` when absent (upstream `snapshot`).
    pub fn snapshot(
        self: &Arc<Self>,
        token: &DocToken,
        owner: DocOwner<'_>,
        context: &Context,
    ) -> SessionFuture<'_, Option<JsonObject>> {
        let definition = Arc::clone(&token.definition);
        let resolved = match resolve_address(&definition, doc_target(&definition, owner)) {
            Ok(resolved) => resolved,
            Err(error) => return error_future(error.into()),
        };
        if let Err(error) = self.assert_usable() {
            return error_future(error);
        }
        let this = Arc::clone(self);
        let context = context.clone();
        Box::pin(async move {
            this.snapshot_resolved(&definition, resolved.id, resolved.address, &context)
                .await
        })
    }

    async fn snapshot_resolved(
        &self,
        definition: &Arc<crate::documents::DocDefinition>,
        address_id: String,
        address: DocumentAddress,
        context: &Context,
    ) -> SessionResult<Option<JsonObject>> {
        if let Some(cached) = self.cached(&address_id)
            && cached.value_version == definition.version
        {
            check_record_scope_record(definition, &cached.record)?;
            check_record_version_record(definition, &cached.record, cached.stored_version())?;
            return Ok(cached.tracker.value().as_object().cloned());
        }
        let _guard = self.line.lock().await;
        self.assert_healthy()?;
        let loaded = self
            .load_document(definition, &address_id, &address, context)
            .await?;
        let Some(loaded) = loaded else {
            return Ok(None);
        };
        check_record_scope_record(definition, &loaded.record)?;
        check_record_version_record(definition, &loaded.record, loaded.stored_version())?;
        Ok(loaded.tracker.value().as_object().cloned())
    }

    /// Attach a read-only replicated state to one document (upstream `documentState`).
    pub fn document_state(
        self: &Arc<Self>,
        token: &DocToken,
        owner: DocOwner<'_>,
        context: &Context,
    ) -> SessionFuture<'_, Option<crate::chord::state::AttachedReplicatedState>> {
        let definition = Arc::clone(&token.definition);
        let resolved = match resolve_address(&definition, doc_target(&definition, owner)) {
            Ok(resolved) => resolved,
            Err(error) => return error_future(error.into()),
        };
        if let Err(error) = self.assert_usable() {
            return error_future(error);
        }
        let this = Arc::clone(self);
        let context = context.clone();
        Box::pin(async move {
            this.document_state_resolved(&definition, resolved.id, resolved.address, &context)
                .await
        })
    }

    async fn document_state_resolved(
        &self,
        definition: &Arc<crate::documents::DocDefinition>,
        address_id: String,
        address: DocumentAddress,
        context: &Context,
    ) -> SessionResult<Option<crate::chord::state::AttachedReplicatedState>> {
        let _guard = self.line.lock().await;
        self.assert_healthy()?;
        let loaded = self
            .load_document(definition, &address_id, &address, context)
            .await?;
        let Some(loaded) = loaded else {
            return Ok(None);
        };
        check_record_scope_record(definition, &loaded.record)?;
        check_record_version_record(definition, &loaded.record, loaded.stored_version())?;
        let source = Arc::new(CommittedStateSource::new(loaded.tracker.value()));
        let observed = Arc::new(AtomicU64::new(loaded.value_version));
        let record_id = loaded.record.id;
        let source_commit = Arc::clone(&source);
        let subscription_commit = self.subscribe_memory(Arc::new(move |changes, context| {
            for change in changes {
                if let CommitChange::Document(document) = change {
                    if document.record.id != record_id {
                        continue;
                    }
                    let ops = observed_operations(&observed, document);
                    if ops.is_empty() {
                        continue;
                    }
                    source_commit.advance(
                        document.frame_value(),
                        ops,
                        without_abort_signal(context),
                    );
                }
            }
        }));
        let source_close = Arc::clone(&source);
        let subscription_close =
            self.subscribe_close(Arc::new(move || source_close.close_session()));
        source.set_release(Box::new(move || {
            subscription_commit.unsubscribe();
            subscription_close.unsubscribe();
        }));
        Ok(Some(attach_state(&source)))
    }

    /// Attach a serialized async watch to one document (upstream `watchDoc`).
    pub fn watch_doc(
        self: &Arc<Self>,
        token: &DocToken,
        owner: DocOwner<'_>,
        context: &Context,
    ) -> SessionFuture<'_, Option<CommittedWatch>> {
        let definition = Arc::clone(&token.definition);
        let resolved = match resolve_address(&definition, doc_target(&definition, owner)) {
            Ok(resolved) => resolved,
            Err(error) => return error_future(error.into()),
        };
        if let Err(error) = self.assert_usable() {
            return error_future(error);
        }
        let signal = context.abort_signal();
        let this = Arc::clone(self);
        let context = context.clone();
        Box::pin(async move {
            this.watch_doc_resolved(&definition, resolved.id, resolved.address, &context, signal)
                .await
        })
    }

    async fn watch_doc_resolved(
        &self,
        definition: &Arc<crate::documents::DocDefinition>,
        address_id: String,
        address: DocumentAddress,
        context: &Context,
        signal: Option<AbortSignal>,
    ) -> SessionResult<Option<CommittedWatch>> {
        if signal.as_ref().is_some_and(|signal| signal.is_cancelled()) {
            return Err(SessionError::Aborted);
        }
        let _guard = self.line.lock().await;
        self.assert_healthy()?;
        if signal.as_ref().is_some_and(|signal| signal.is_cancelled()) {
            return Err(SessionError::Aborted);
        }
        let loaded = self
            .load_document(definition, &address_id, &address, context)
            .await?;
        if signal.as_ref().is_some_and(|signal| signal.is_cancelled()) {
            return Err(SessionError::Aborted);
        }
        let Some(loaded) = loaded else {
            return Ok(None);
        };
        check_record_scope_record(definition, &loaded.record)?;
        check_record_version_record(definition, &loaded.record, loaded.stored_version())?;
        let watch = CommittedWatch::new(loaded.tracker.value());
        let observed = Arc::new(AtomicU64::new(loaded.value_version));
        let record_id = loaded.record.id;
        let watch_commit = watch.clone();
        let subscription_commit = self.subscribe_memory(Arc::new(move |changes, context| {
            for change in changes {
                if let CommitChange::Document(document) = change {
                    if document.record.id != record_id {
                        continue;
                    }
                    let ops = observed_operations(&observed, document);
                    if ops.is_empty() {
                        continue;
                    }
                    watch_commit.advance(document.frame_value(), ops, context.clone());
                }
            }
        }));
        let watch_close = watch.clone();
        let subscription_close =
            self.subscribe_close(Arc::new(move || watch_close.close_session()));
        watch.set_detach(Box::new(move || {
            subscription_commit.unsubscribe();
            subscription_close.unsubscribe();
        }));
        if let Some(signal) = signal {
            if signal.is_cancelled() {
                watch.cancel();
                return Err(SessionError::Aborted);
            }
            let watch_cancel = watch.clone();
            tokio::spawn(async move {
                signal.cancelled().await;
                watch_cancel.cancel();
            });
        }
        Ok(Some(watch))
    }

    /// Historical read of one conversation document (upstream `snapshotAsOf`).
    pub fn snapshot_as_of(
        self: &Arc<Self>,
        token: &DocToken,
        conversation: ConversationId,
        at: EntryId,
        context: &Context,
    ) -> SessionFuture<'_, Option<JsonObject>> {
        let definition = Arc::clone(&token.definition);
        if !matches!(definition.scope, DocScope::Conversation) {
            return error_future(SessionError::message(
                "Session.snapshot_as_of() requires a conversation document",
            ));
        }
        if let Err(error) = self.assert_usable() {
            return error_future(error);
        }
        let this = Arc::clone(self);
        let context = context.clone();
        Box::pin(async move {
            this.snapshot_as_of_resolved(&definition, conversation, at, &context)
                .await
        })
    }

    async fn snapshot_as_of_resolved(
        &self,
        definition: &Arc<crate::documents::DocDefinition>,
        conversation: ConversationId,
        at: EntryId,
        context: &Context,
    ) -> SessionResult<Option<JsonObject>> {
        let _guard = self.line.lock().await;
        self.assert_healthy()?;
        let stored_entry = match self
            .storage
            .entry_in_conversation(conversation, at, context)
            .await
        {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                return Err(SessionError::message(format!(
                    "Entry {at} is not visible from conversation {conversation}"
                )));
            }
            Err(error) => {
                self.fail_unexpected_storage(&error, Some(context));
                return Err(error.into());
            }
        };
        let address = DocumentAddress {
            kind: definition.kind.clone(),
            scope: DocumentScope::Conversation {
                conversation_id: stored_entry.entry.conversation_id,
            },
            key: None,
        };
        let point = crate::types::DocumentPoint::At(stored_entry.commit_seq);
        let record = match self.storage.find_document(address, point, context).await {
            Ok(record) => record,
            Err(error) => {
                self.fail_unexpected_storage(&error, Some(context));
                return Err(error.into());
            }
        };
        let Some(record) = record else {
            return Ok(None);
        };
        let stored = match self.storage.document(record.id, point, context).await {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                return Err(SessionError::message(format!(
                    "Historical document {} ({}) cannot be read",
                    record.id, record.kind
                )));
            }
            Err(error) => {
                self.fail_unexpected_storage(&error, Some(context));
                return Err(error.into());
            }
        };
        Ok(Some(materialize_document(definition, &stored)?))
    }

    /// Current conversation document resolved on the line (upstream
    /// `conversationDocumentOnLine`).
    pub async fn conversation_document_on_line(
        &self,
        token: &DocToken,
        conversation_id: ConversationId,
        context: &Context,
    ) -> SessionResult<Option<ConversationDocumentView>> {
        let definition = Arc::clone(&token.definition);
        let target = DocTarget::Conversation {
            conversation_id,
            key: None,
        };
        let resolved = resolve_address(&definition, target)?;
        let loaded = self
            .load_document(&definition, &resolved.id, &resolved.address, context)
            .await?;
        let Some(loaded) = loaded else {
            return Ok(None);
        };
        check_record_scope_record(&definition, &loaded.record)?;
        check_record_version_record(&definition, &loaded.record, loaded.stored_version())?;
        Ok(Some(ConversationDocumentView {
            record: loaded.record.clone(),
            version: loaded.value_version,
            value: loaded
                .tracker
                .value()
                .as_object()
                .cloned()
                .unwrap_or_default(),
        }))
    }

    // ─── Cache ──────────────────────────────────────────────────────────────

    /// Drop every loaded tracker on the line (upstream `unloadDocuments`).
    pub fn unload_documents(self: &Arc<Self>) -> SessionFuture<'_, ()> {
        let this = Arc::clone(self);
        Box::pin(async move {
            let _guard = this.line.lock().await;
            let context = crate::chord::context::BACKGROUND_CONTEXT.clone();
            if this.fatal_cause().is_none() {
                this.store_holds(&context).await?;
            }
            if let Ok(mut documents) = this.documents.lock() {
                documents.clear();
            }
            Ok(())
        })
    }

    // ─── Close ──────────────────────────────────────────────────────────────

    /// Seal admission and close storage (upstream `close`).
    pub fn close(self: &Arc<Self>, context: &Context) -> SessionFuture<'_, ()> {
        if !self.closing.swap(true, Ordering::SeqCst) {
            let listeners: Vec<CloseListener> = match self.close_listeners.lock() {
                Ok(mut listeners) => listeners.drain(..).map(|(_, listener)| listener).collect(),
                Err(_) => Vec::new(),
            };
            for listener in listeners {
                listener();
            }
        }
        let this = Arc::clone(self);
        let context = without_abort_signal(context);
        Box::pin(async move {
            let _guard = this.line.lock().await;
            if this.cleanup_done.swap(true, Ordering::SeqCst) {
                return match this.fatal_cause() {
                    Some(cause) => Err(SessionError::SessionFailed(cause)),
                    None => match this.close_error.lock() {
                        Ok(error) => match error.as_ref() {
                            Some(message) => Err(SessionError::message(message.clone())),
                            None => Ok(()),
                        },
                        Err(_) => Err(SessionError::message("Session close outcome lock poisoned")),
                    },
                };
            }
            // Cancellation before completion leaves close retryable; completed failure is sealed.
            let mut reset = ResetOnDrop::new(&this.cleanup_done);
            if let Ok(mut timer) = this.hold_timer.lock()
                && let Some((_, disarm)) = timer.take()
            {
                disarm();
            }
            let persistence_error = if this.fatal_cause().is_some() {
                None
            } else {
                this.store_holds(&context).await.err()
            };
            if let Ok(mut documents) = this.documents.lock() {
                documents.clear();
            }
            if let Ok(mut listeners) = this.commit_listeners.lock() {
                listeners.clear();
            }
            if let Ok(mut listeners) = this.memory_listeners.lock() {
                listeners.clear();
            }
            if let Err(error) = this.storage.close(&context).await {
                this.fail_session(error.to_string());
                reset.disarm();
                return Err(SessionError::Storage(error));
            }
            reset.disarm();
            if let Some(error) = persistence_error {
                if let Ok(mut outcome) = this.close_error.lock() {
                    *outcome = Some(error.to_string());
                }
                return Err(error);
            }
            Ok(())
        })
    }

    // ─── Listeners ──────────────────────────────────────────────────────────

    /// Register a synchronous post-adoption commit listener (upstream `subscribeCommits`).
    pub fn subscribe_commits(&self, listener: CommitListener) -> Subscription {
        let store = Arc::clone(&self.commit_listeners);
        let id = NEXT_LISTENER_ID.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut listeners) = store.lock() {
            listeners.push((id, listener));
        }
        Subscription::new(move || {
            if let Ok(mut listeners) = store.lock() {
                listeners.retain(|(existing, _)| *existing != id);
            }
        })
    }
    /// Register an observer for every adopted document revision, stored or deferred.
    pub fn subscribe_memory(&self, listener: MemoryCommitListener) -> Subscription {
        let store = Arc::clone(&self.memory_listeners);
        let id = NEXT_LISTENER_ID.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut listeners) = store.lock() {
            listeners.push((id, listener));
        }
        Subscription::new(move || {
            if let Ok(mut listeners) = store.lock() {
                listeners.retain(|(existing, _)| *existing != id);
            }
        })
    }

    /// Register a synchronous close listener (upstream `subscribeClose`).
    pub fn subscribe_close(&self, listener: CloseListener) -> Subscription {
        let store = Arc::clone(&self.close_listeners);
        let id = NEXT_LISTENER_ID.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut listeners) = store.lock() {
            listeners.push((id, listener));
        }
        Subscription::new(move || {
            if let Ok(mut listeners) = store.lock() {
                listeners.retain(|(existing, _)| *existing != id);
            }
        })
    }

    fn assert_usable(&self) -> SessionResult<()> {
        if self.closing.load(Ordering::SeqCst) {
            return self.assert_healthy().and(Err(SessionError::Closed));
        }
        self.assert_healthy()
    }

    fn assert_healthy(&self) -> SessionResult<()> {
        let cause = self
            .fatal_cause
            .lock()
            .map_err(|_| SessionError::message("Session failure lock poisoned"))?
            .clone();
        match cause {
            Some(cause) => Err(SessionError::SessionFailed(cause)),
            None => Ok(()),
        }
    }

    fn fail_unexpected_storage(&self, error: &StorageError, context: Option<&Context>) {
        if error.is_request_error()
            || matches!(
                error,
                StorageError::File(file)
                    if file.code == crate::errors::FileErrorCode::Aborted
            )
            || context
                .and_then(Context::abort_signal)
                .is_some_and(|signal| signal.is_cancelled())
        {
            return;
        }
        self.fail_session(error.to_string());
    }

    fn fail_session(&self, cause: String) {
        let first_cause = if let Ok(mut fatal_cause) = self.fatal_cause.lock() {
            if fatal_cause.is_none() {
                *fatal_cause = Some(cause);
            }
            fatal_cause.clone()
        } else {
            Some(cause)
        };
        self.closing.store(true, Ordering::SeqCst);
        let listeners = self
            .close_listeners
            .lock()
            .map(|mut listeners| {
                listeners
                    .drain(..)
                    .map(|(_, listener)| listener)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for listener in listeners {
            listener();
        }
        if let Some(cause) = first_cause
            && let Ok(mut outcome) = self.close_error.lock()
        {
            *outcome = Some(cause);
        }
        let weak = self.self_weak.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Some(session) = weak.upgrade() {
                    let _ = session
                        .close(&crate::chord::context::BACKGROUND_CONTEXT)
                        .await;
                }
            });
        }
    }
    pub(crate) fn fatal_cause(&self) -> Option<String> {
        self.fatal_cause.lock().ok().and_then(|cause| cause.clone())
    }
}

/// Install a watch listener helper used by tests and consumers.
pub fn watch_listener<F>(function: F) -> WatchListener
where
    F: Fn(
            crate::session::ObservedDocumentValue,
            Vec<crate::chord::delta::Op>,
            Context,
        ) -> crate::session::WatchFuture
        + Send
        + Sync
        + 'static,
{
    Arc::new(function)
}
