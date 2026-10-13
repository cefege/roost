//! The seams the coordinator implements: durable storage, worker tool
//! execution, transcript event delivery, and model access. Every method is a
//! `BoxFuture` so the coordinator's database and link types stay out of here.

use std::collections::BTreeMap;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use roost_llm::web_search::{ParsedQuery, SearchQuery, SearchResponse};
use roost_llm::{
    Answer, Catalog, ChatRequest, LlmError, ModelInfo, Question, ResolvedAuth, StreamEvent,
    UsageWindow,
};
use roost_protocol::wire::agent_chat::{ChatEvent, ConversationSummary};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::records::{AgentSettings, ConversationRecord, Entry};

/// The lease a replica holds on a conversation's run. Every journal write the
/// run makes carries it; a write under a superseded epoch is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseFence {
    pub owner: String,
    pub epoch: u64,
}

/// The run-intent row a command appends after its entries, in the same
/// transaction, so input and the decision to run it are one commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentChange {
    None,
    /// An active intent through the last entry this command appended.
    Activate,
    /// An inactive, aborted intent through the journal's last entry.
    Abort,
}

/// One client command, deduplicated by `(conversation, request_id)`.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandAdmission {
    pub request_id: String,
    pub kind: String,
    pub entries: Vec<Entry>,
    pub intent: IntentChange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitOutcome {
    /// The sequence of each appended entry, the intent last.
    Admitted { seqs: Vec<u64> },
    /// The request was admitted before; `result_json` is what completed it,
    /// if anything has.
    Duplicate { result_json: Option<String> },
}

pub trait AgentStore: Send + Sync {
    fn conversations(&self) -> BoxFuture<'_, Result<Vec<ConversationRecord>, AgentError>>;
    fn conversation<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ConversationRecord>, AgentError>>;
    /// Compare-and-set: `expected_revision` 0 inserts a new row, any other
    /// value updates only that revision. Returns the stored revision;
    /// `AgentError::Conflict` when another writer got there first.
    fn save_conversation<'a>(
        &'a self,
        record: &'a ConversationRecord,
        expected_revision: u64,
    ) -> BoxFuture<'a, Result<u64, AgentError>>;
    /// Deletes the conversation, its entries and every descendant conversation.
    fn delete_conversation<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), AgentError>>;
    /// Appends an entry and returns its sequence number (strictly increasing per conversation).
    /// Checks cancellation and, when `fence` is set, that the caller still
    /// holds the run lease, before committing. Once commit starts, callers must
    /// await its result: dropping the future cannot undo a database commit.
    fn append_entry<'a>(
        &'a self,
        id: &'a str,
        entry: &'a Entry,
        cancellation: &'a [&'a CancellationToken],
        fence: Option<&'a LeaseFence>,
    ) -> BoxFuture<'a, Result<u64, AgentError>>;
    /// Appends a command's entries, its intent and its dedup row in one
    /// transaction, or reports the earlier admission of the same request.
    fn admit_command<'a>(
        &'a self,
        id: &'a str,
        admission: CommandAdmission,
    ) -> BoxFuture<'a, Result<AdmitOutcome, AgentError>>;
    /// Records what a command produced, for a retried request to return.
    fn complete_command<'a>(
        &'a self,
        id: &'a str,
        request_id: &'a str,
        result_json: &'a str,
    ) -> BoxFuture<'a, Result<(), AgentError>>;
    /// Takes the run lease when it is free, expired, or already this owner's
    /// (a restarted owner); returns the new epoch, or `None` while another
    /// owner's lease is live.
    fn acquire_lease<'a>(
        &'a self,
        id: &'a str,
        owner: &'a str,
        ttl_ms: u64,
    ) -> BoxFuture<'a, Result<Option<u64>, AgentError>>;
    /// Extends a held lease; `false` once another owner has taken it.
    fn renew_lease<'a>(
        &'a self,
        id: &'a str,
        fence: &'a LeaseFence,
        ttl_ms: u64,
    ) -> BoxFuture<'a, Result<bool, AgentError>>;
    /// Marks a held lease free without forgetting its epoch.
    fn release_lease<'a>(
        &'a self,
        id: &'a str,
        fence: &'a LeaseFence,
    ) -> BoxFuture<'a, Result<(), AgentError>>;
    /// Conversations whose run may have lost its owner: a lease that expired
    /// without release, or a running row with no live lease.
    fn orphaned_runs(&self, now_ms: i64) -> BoxFuture<'_, Result<Vec<String>, AgentError>>;
    fn entries<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<(u64, Entry)>, AgentError>>;
    fn settings(&self) -> BoxFuture<'_, Result<AgentSettings, AgentError>>;
    fn save_settings<'a>(
        &'a self,
        settings: &'a AgentSettings,
    ) -> BoxFuture<'a, Result<(), AgentError>>;
    /// The advisor's persisted review state for a conversation, as JSON.
    fn advisor_state<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, AgentError>>;
    fn save_advisor_state<'a>(
        &'a self,
        id: &'a str,
        state_json: &'a str,
    ) -> BoxFuture<'a, Result<(), AgentError>>;
}

/// One worker tool invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub call_id: String,
    /// Keys the worker's per-conversation tool state (the hashline edit store).
    pub conversation_id: String,
    pub cwd: String,
    pub tool: String,
    pub args_json: String,
    pub timeout_ms: u32,
    /// The turn (user-entry seq) this call belongs to; 0 outside a turn.
    pub checkpoint_id: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolOutcome {
    pub is_error: bool,
    pub content: String,
    pub details_json: String,
}

pub trait ToolExecutor: Send + Sync {
    /// Runs a tool on the worker `worker_fp`, streaming live output chunks to
    /// `out`. `Err` is a transport failure (machine offline, link lost).
    fn execute<'a>(
        &'a self,
        worker_fp: &'a str,
        call: ToolCall,
        out: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolOutcome, String>>;
    /// Drops the worker's per-conversation tool state.
    fn close_conversation<'a>(
        &'a self,
        worker_fp: &'a str,
        conversation_id: &'a str,
    ) -> BoxFuture<'a, ()>;
}

/// Delivery of conversation rows and transcript events to clients.
pub trait ChatSink: Send + Sync {
    fn conversation<'a>(&'a self, summary: &'a ConversationSummary) -> BoxFuture<'a, ()>;
    fn events<'a>(&'a self, conversation_id: &'a str, events: Vec<ChatEvent>) -> BoxFuture<'a, ()>;
    fn removed<'a>(&'a self, conversation_id: &'a str) -> BoxFuture<'a, ()>;
}

/// Wakes the replica that owns a conversation's run after durable input, an
/// abort or a deletion was committed elsewhere. Delivery is best-effort; the
/// journal is the truth and the receiver re-reads it.
pub trait ClusterSignal: Send + Sync {
    fn conversation_changed<'a>(&'a self, conversation_id: &'a str) -> BoxFuture<'a, ()>;
}

/// The signal of a process that is the only replica: there is no one to wake.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoClusterSignal;

impl ClusterSignal for NoClusterSignal {
    fn conversation_changed<'a>(&'a self, _conversation_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

/// One account's row in `/usage`.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsage {
    pub provider: String,
    pub label: String,
    pub windows: Vec<UsageWindow>,
    pub note: Option<String>,
    pub blocked_until_ms: Option<i64>,
    pub disabled_cause: Option<String>,
}

/// Model access: catalog, accounts, streaming, judgments, and coordinator web requests.
pub trait Llm: Send + Sync {
    fn catalog(&self) -> &Catalog;
    fn has_credential<'a>(&'a self, provider: &'a str) -> BoxFuture<'a, bool>;
    fn resolve<'a>(
        &'a self,
        provider: &'a str,
        conversation_id: &'a str,
    ) -> BoxFuture<'a, Result<(i64, ResolvedAuth), LlmError>>;
    fn rotate<'a>(
        &'a self,
        credential_id: i64,
        provider: &'a str,
        conversation_id: &'a str,
        error: &'a LlmError,
    ) -> BoxFuture<'a, Result<(i64, ResolvedAuth), LlmError>>;
    fn stream(
        &self,
        credential_id: i64,
        request: ChatRequest,
        auth: ResolvedAuth,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<StreamEvent, LlmError>>;
    fn judge<'a>(
        &'a self,
        model: &'a ModelInfo,
        state: serde_json::Value,
        questions: Vec<Question>,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Answer>, LlmError>>;
    fn account_usage(&self) -> BoxFuture<'_, Vec<AccountUsage>>;
    fn fetch_url<'a>(&'a self, url: &'a str)
    -> BoxFuture<'a, Result<crate::FetchedPage, LlmError>>;
    fn search_engine<'a>(
        &'a self,
        engine_id: &'a str,
        base_url: Option<&'a str>,
        query: &'a SearchQuery,
        parsed: &'a ParsedQuery,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<SearchResponse, LlmError>>;
    fn engine_configured<'a>(&'a self, engine_id: &'a str) -> BoxFuture<'a, bool>;
    fn hosted_search<'a>(
        &'a self,
        model: &'a ModelInfo,
        conversation_id: &'a str,
        query: &'a SearchQuery,
        parsed: &'a ParsedQuery,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<SearchResponse, LlmError>>;
}
