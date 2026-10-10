//! The harness runtime: shared state behind a cheap-to-clone handle. It owns
//! the per-conversation run registry and record locks; `api` holds the
//! coordinator-facing operations and `runs` the run lifecycle.

mod api;
mod runs;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use roost_protocol::wire::agent_chat::{ChatEvent, ConversationSummary, UsageTotals};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::advisor::AdvisorHub;
use crate::error::AgentError;
use crate::projection;
use crate::records::{ConversationRecord, Entry};
use crate::traits::{AgentStore, ChatSink, Llm, ToolExecutor};

pub use api::NewConversation;
pub(crate) use runs::{RunEnd, Steer};

/// Tunables; tests shorten the waits.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Waits before each retry of a transient provider failure.
    pub retry_backoff: Vec<Duration>,
    /// Waits before each retry of a failed advisor review.
    pub advisor_backoff: Vec<Duration>,
    /// Upper bound on the semantic `find` cascade.
    pub find_timeout: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            retry_backoff: vec![
                Duration::from_secs(2),
                Duration::from_secs(10),
                Duration::from_secs(60),
            ],
            advisor_backoff: vec![
                Duration::from_secs(2),
                Duration::from_secs(8),
                Duration::from_secs(30),
            ],
            find_timeout: Duration::from_secs(20),
        }
    }
}

/// The agent harness. Clone freely; every clone shares one state.
#[derive(Clone)]
pub struct AgentRuntime {
    pub(crate) inner: Arc<RuntimeInner>,
}

impl std::fmt::Debug for AgentRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentRuntime")
            .finish_non_exhaustive()
    }
}

pub(crate) struct RuntimeInner {
    pub(crate) store: Arc<dyn AgentStore>,
    pub(crate) tools: Arc<dyn ToolExecutor>,
    pub(crate) sink: Arc<dyn ChatSink>,
    pub(crate) llm: Arc<dyn Llm>,
    pub(crate) config: RuntimeConfig,
    runs: Mutex<HashMap<String, RunHandle>>,
    /// Whether each conversation's latest run ended by a user abort.
    aborted_last: Mutex<HashMap<String, bool>>,
    record_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    context_cache:
        Mutex<HashMap<String, (Instant, roost_protocol::wire::agent_chat::ContextFiles)>>,
    extra_usage: Mutex<HashMap<String, UsageTotals>>,
    /// Shared `task` context of each live child conversation.
    pub(crate) child_context: Mutex<HashMap<String, String>>,
    pub(crate) advisors: AdvisorHub,
}

pub(crate) struct RunHandle {
    pub(crate) cancel: CancellationToken,
    pub(crate) steer: mpsc::UnboundedSender<Steer>,
    pub(crate) user_aborted: Arc<AtomicBool>,
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// A random lowercase-hex identifier with a readable prefix.
pub(crate) fn random_id(prefix: &str) -> String {
    let mut bytes = [0_u8; 12];
    if getrandom::fill(&mut bytes).is_err() {
        // Uniqueness, not secrecy, is required here; the clock is a fallback.
        bytes[..8].copy_from_slice(&now_ms().to_le_bytes());
    }
    format!("{prefix}{}", hex::encode(bytes))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AgentRuntime {
    pub fn new(
        store: Arc<dyn AgentStore>,
        tools: Arc<dyn ToolExecutor>,
        sink: Arc<dyn ChatSink>,
        llm: Arc<dyn Llm>,
        config: RuntimeConfig,
    ) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                store,
                tools,
                sink,
                llm,
                config,
                runs: Mutex::new(HashMap::new()),
                aborted_last: Mutex::new(HashMap::new()),
                record_locks: Mutex::new(HashMap::new()),
                context_cache: Mutex::new(HashMap::new()),
                extra_usage: Mutex::new(HashMap::new()),
                child_context: Mutex::new(HashMap::new()),
                advisors: AdvisorHub::default(),
            }),
        }
    }

    pub(crate) async fn record(&self, id: &str) -> Result<ConversationRecord, AgentError> {
        self.inner
            .store
            .conversation(id)
            .await?
            .ok_or_else(|| AgentError::NotFound(id.to_owned()))
    }

    /// Read-modify-write of a conversation row under its lock, then publish.
    pub(crate) async fn update_record(
        &self,
        id: &str,
        change: impl FnOnce(&mut ConversationRecord),
    ) -> Result<ConversationRecord, AgentError> {
        let record_lock = {
            let mut locks = lock(&self.inner.record_locks);
            Arc::clone(locks.entry(id.to_owned()).or_default())
        };
        let _guard = record_lock.lock().await;
        let mut record = self.record(id).await?;
        change(&mut record);
        record.updated_ms = now_ms();
        self.inner.store.save_conversation(&record).await?;
        self.publish_summary(&record).await;
        Ok(record)
    }

    pub(crate) async fn summary(&self, record: &ConversationRecord) -> ConversationSummary {
        let advisor_default = self
            .inner
            .store
            .settings()
            .await
            .map(|settings| settings.advisor_enabled)
            .unwrap_or(false);
        ConversationSummary {
            id: record.id.clone(),
            title: record.title.clone(),
            worker_fp: record.worker_fp.clone(),
            worker_label: record.worker_label.clone(),
            cwd: record.cwd.clone(),
            model: record.model.clone(),
            thinking_level: Some(record.thinking_level.clone()),
            mode: record.mode.as_str().to_owned(),
            parent_id: record.parent_id.clone(),
            agent: record.agent.clone(),
            advisor: record.parent_id.is_none() && record.advisor.unwrap_or(advisor_default),
            run_state: record.run_state,
            error: record.error.clone(),
            created_ms: record.created_ms,
            updated_ms: record.updated_ms,
        }
    }

    pub(crate) async fn publish_summary(&self, record: &ConversationRecord) {
        let summary = self.summary(record).await;
        self.inner.sink.conversation(&summary).await;
    }

    pub(crate) async fn emit(&self, id: &str, events: Vec<ChatEvent>) {
        self.inner.sink.events(id, events).await;
    }

    /// Appends an entry and emits the transcript item it projects to.
    pub(crate) async fn append(&self, id: &str, entry: Entry) -> Result<u64, AgentError> {
        let seq = self.inner.store.append_entry(id, &entry).await?;
        let tools = if matches!(entry, Entry::ToolResult { .. }) {
            projection::index_tool_calls(&self.inner.store.entries(id).await?)
        } else {
            projection::ToolCallIndex::new()
        };
        if let Some(item) = projection::entry_item(seq, &entry, &tools) {
            self.emit(id, vec![ChatEvent::Item { item }]).await;
        }
        Ok(seq)
    }

    pub(crate) async fn notice(&self, id: &str, level: &str, title: &str, body: &str) {
        let entry = Entry::Notice {
            level: level.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
        };
        if let Err(error) = self.append(id, entry).await {
            tracing::warn!(conversation_id = id, %error, "agent notice was not recorded");
        }
    }

    /// Adds usage from a model call that has no assistant entry of its own.
    pub(crate) async fn add_extra_usage(
        &self,
        id: &str,
        provider: &str,
        model_id: &str,
        usage: &roost_llm::Usage,
    ) {
        {
            let mut extra = lock(&self.inner.extra_usage);
            let totals = extra.entry(id.to_owned()).or_default();
            projection::add_usage(totals, self.inner.llm.catalog(), provider, model_id, usage);
        }
        if let Ok(totals) = self.usage_totals(id).await {
            self.emit(id, vec![ChatEvent::Usage { usage: totals }])
                .await;
        }
    }

    pub(crate) async fn usage_totals(&self, id: &str) -> Result<UsageTotals, AgentError> {
        let entries = self.inner.store.entries(id).await?;
        let extra = lock(&self.inner.extra_usage)
            .get(id)
            .cloned()
            .unwrap_or_default();
        Ok(projection::usage_totals(
            self.inner.llm.catalog(),
            &entries,
            &extra,
        ))
    }

    pub(crate) fn is_running(&self, id: &str) -> bool {
        lock(&self.inner.runs).contains_key(id)
    }

    pub(crate) fn last_run_aborted(&self, id: &str) -> bool {
        lock(&self.inner.aborted_last)
            .get(id)
            .copied()
            .unwrap_or(false)
    }

    pub(crate) fn cached_context(
        &self,
        key: &str,
        max_age: Duration,
    ) -> Option<roost_protocol::wire::agent_chat::ContextFiles> {
        lock(&self.inner.context_cache)
            .get(key)
            .filter(|(fetched, _)| fetched.elapsed() < max_age)
            .map(|(_, files)| files.clone())
    }

    pub(crate) fn store_context(
        &self,
        key: &str,
        files: roost_protocol::wire::agent_chat::ContextFiles,
    ) {
        lock(&self.inner.context_cache).insert(key.to_owned(), (Instant::now(), files));
    }

    pub(crate) fn child_context(&self, id: &str) -> Option<String> {
        lock(&self.inner.child_context).get(id).cloned()
    }

    pub(crate) fn set_child_context(&self, id: &str, context: String) {
        lock(&self.inner.child_context).insert(id.to_owned(), context);
    }

    fn mark_aborted(&self, id: &str, aborted: bool) {
        lock(&self.inner.aborted_last).insert(id.to_owned(), aborted);
    }

    fn take_run(&self, id: &str) -> Option<RunHandle> {
        lock(&self.inner.runs).remove(id)
    }

    fn insert_run(&self, id: &str, handle: RunHandle) -> bool {
        let mut runs = lock(&self.inner.runs);
        if runs.contains_key(id) {
            return false;
        }
        runs.insert(id.to_owned(), handle);
        true
    }

    fn with_run<T>(&self, id: &str, read: impl FnOnce(&RunHandle) -> T) -> Option<T> {
        lock(&self.inner.runs).get(id).map(read)
    }

    fn user_abort(&self, id: &str) -> bool {
        self.with_run(id, |run| {
            run.user_aborted.store(true, Ordering::SeqCst);
            run.cancel.cancel();
        })
        .is_some()
    }
}

pub(crate) fn new_abort_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
