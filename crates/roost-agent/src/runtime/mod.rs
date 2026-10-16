//! The harness runtime: shared state behind a cheap-to-clone handle. It owns
//! the per-conversation run registry, run leases and record locks; `api`
//! holds the coordinator-facing operations and `runs` the run lifecycle.

mod abort;
mod api;
mod dedup;
mod lease;
mod publishing;
mod readmission;
mod recovered_turn;
mod recovery;
mod remote;
mod runs;
mod steering;
mod supervision;
mod tool_recovery;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::advisor::AdvisorHub;
use crate::error::AgentError;
use crate::loop_mode::LoopState;
use crate::records::ConversationRecord;
use crate::traits::{
    AgentStore, ChatSink, ClusterSignal, LeaseFence, Llm, NoClusterSignal, ToolExecutor,
};

pub use api::NewConversation;
pub(crate) use recovery::latest_submission_seq;
pub(crate) use runs::{RunEnd, Steer};
pub(crate) use steering::CommandKey;

/// Tunables; tests shorten the waits.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Waits before each retry of a transient provider failure.
    pub retry_backoff: Vec<Duration>,
    /// Waits before each retry of a failed advisor review.
    pub advisor_backoff: Vec<Duration>,
    /// Upper bound on the semantic `find` cascade.
    pub find_timeout: Duration,
    /// This replica's run-lease owner name; unique per process.
    pub instance_id: String,
    /// How long a run lease lives without renewal.
    pub lease_ttl: Duration,
    /// How often a running conversation renews its lease.
    pub lease_renew: Duration,
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
            instance_id: random_id("coord-"),
            lease_ttl: Duration::from_secs(30),
            lease_renew: Duration::from_secs(10),
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
    pub(crate) signal: Arc<dyn ClusterSignal>,
    pub(crate) config: RuntimeConfig,
    runs: Mutex<HashMap<String, RunHandle>>,
    /// The run leases this replica holds, by conversation. A fence stays here
    /// after another replica takes the lease, so the run's later writes keep
    /// failing until it settles.
    leases: Mutex<HashMap<String, LeaseFence>>,
    /// Whether each conversation's latest run ended by a user abort.
    aborted_last: Mutex<HashMap<String, bool>>,
    record_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    lifecycle_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Wakes a `wait` call when a background job settles or a message lands for a
    /// conversation. Entries are kept like `lifecycle_locks`: one per conversation id.
    arrivals: Mutex<HashMap<String, Arc<tokio::sync::Notify>>>,
    goal_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    goal_clocks: Mutex<HashMap<String, (String, Instant)>>,
    context_cache:
        Mutex<HashMap<String, (Instant, roost_protocol::wire::agent_chat::ContextFiles)>>,
    pub(crate) advisors: AdvisorHub,
    /// Per-conversation `/loop` state, restored from durable journal snapshots.
    /// The map owns the active driver's state while the runtime is alive.
    pub(crate) loops: Mutex<HashMap<String, LoopState>>,
}

pub(crate) struct RunHandle {
    pub(crate) cancel: CancellationToken,
    pub(crate) steer: mpsc::UnboundedSender<Steer>,
    pub(crate) user_aborted: Arc<AtomicBool>,
    pub(crate) processed_seq: Arc<AtomicU64>,
    pub(crate) abort_seq: Arc<AtomicU64>,
    /// The highest input sequence handed to `steer`.
    pub(crate) queued_seq: Arc<AtomicU64>,
    /// The active intent this run started under; a later aborted intent
    /// written by another replica stops it.
    pub(crate) start_intent_seq: Arc<AtomicU64>,
    /// Set when another replica took the lease: the run stops without
    /// writing a settlement.
    pub(crate) fenced: Arc<AtomicBool>,
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
    /// A runtime that is the only replica over its store.
    pub fn new(
        store: Arc<dyn AgentStore>,
        tools: Arc<dyn ToolExecutor>,
        sink: Arc<dyn ChatSink>,
        llm: Arc<dyn Llm>,
        config: RuntimeConfig,
    ) -> Self {
        Self::clustered(store, tools, sink, llm, Arc::new(NoClusterSignal), config)
    }

    /// A runtime that shares its store with other replicas and wakes them
    /// through `signal`.
    pub fn clustered(
        store: Arc<dyn AgentStore>,
        tools: Arc<dyn ToolExecutor>,
        sink: Arc<dyn ChatSink>,
        llm: Arc<dyn Llm>,
        signal: Arc<dyn ClusterSignal>,
        config: RuntimeConfig,
    ) -> Self {
        tracing::info!(instance_id = %config.instance_id, "agent runtime started");
        Self {
            inner: Arc::new(RuntimeInner {
                store,
                tools,
                sink,
                llm,
                signal,
                config,
                runs: Mutex::new(HashMap::new()),
                leases: Mutex::new(HashMap::new()),
                aborted_last: Mutex::new(HashMap::new()),
                record_locks: Mutex::new(HashMap::new()),
                lifecycle_locks: Mutex::new(HashMap::new()),
                arrivals: Mutex::new(HashMap::new()),
                goal_locks: Mutex::new(HashMap::new()),
                goal_clocks: Mutex::new(HashMap::new()),
                context_cache: Mutex::new(HashMap::new()),
                advisors: AdvisorHub::default(),
                loops: Mutex::new(HashMap::new()),
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
    /// A concurrent write from another replica is re-read and `change`
    /// applied again, so neither writer's fields are lost.
    pub(crate) async fn update_record(
        &self,
        id: &str,
        mut change: impl FnMut(&mut ConversationRecord),
    ) -> Result<ConversationRecord, AgentError> {
        let record_lock = {
            let mut locks = lock(&self.inner.record_locks);
            Arc::clone(locks.entry(id.to_owned()).or_default())
        };
        let _guard = record_lock.lock().await;
        let mut attempts = 0_u32;
        loop {
            let mut record = self.record(id).await?;
            change(&mut record);
            record.updated_ms = now_ms();
            match self
                .inner
                .store
                .save_conversation(&record, record.revision)
                .await
            {
                Ok(revision) => {
                    record.revision = revision;
                    self.publish_summary(&record).await;
                    return Ok(record);
                }
                Err(AgentError::Conflict(_)) if attempts < 8 => {
                    attempts += 1;
                    tracing::debug!(
                        conversation_id = id,
                        attempts,
                        "conversation row changed concurrently; retrying"
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn lifecycle_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            lock(&self.inner.lifecycle_locks)
                .entry(id.to_owned())
                .or_default(),
        )
    }
    pub(crate) fn arrival_signal(&self, id: &str) -> Arc<tokio::sync::Notify> {
        Arc::clone(lock(&self.inner.arrivals).entry(id.to_owned()).or_default())
    }
    pub(crate) fn goal_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            lock(&self.inner.goal_locks)
                .entry(id.to_owned())
                .or_default(),
        )
    }
    pub(crate) fn goal_clocks(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, (String, Instant)>> {
        lock(&self.inner.goal_clocks)
    }

    pub(crate) fn note_processed_seq(&self, id: &str, sequence: u64) {
        let _ = self.with_run(id, |run| {
            run.processed_seq.fetch_max(sequence, Ordering::SeqCst);
        });
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

    pub(crate) fn with_run<T>(&self, id: &str, read: impl FnOnce(&RunHandle) -> T) -> Option<T> {
        lock(&self.inner.runs).get(id).map(read)
    }
}

pub(crate) fn new_abort_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
