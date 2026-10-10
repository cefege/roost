//! The seams the coordinator implements: durable storage, worker tool
//! execution, transcript event delivery, and model access. Every method is a
//! `BoxFuture` so the coordinator's database and link types stay out of here.

use std::collections::BTreeMap;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use roost_llm::{
    Answer, Catalog, ChatRequest, LlmError, ModelInfo, Question, ResolvedAuth, StreamEvent,
    UsageWindow,
};
use roost_protocol::wire::agent_chat::{ChatEvent, ConversationSummary};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::records::{AgentSettings, ConversationRecord, Entry};

pub trait AgentStore: Send + Sync {
    fn conversations(&self) -> BoxFuture<'_, Result<Vec<ConversationRecord>, AgentError>>;
    fn conversation<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ConversationRecord>, AgentError>>;
    fn save_conversation<'a>(
        &'a self,
        record: &'a ConversationRecord,
    ) -> BoxFuture<'a, Result<(), AgentError>>;
    /// Deletes the conversation, its entries and every descendant conversation.
    fn delete_conversation<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), AgentError>>;
    /// Appends an entry and returns its sequence number (strictly increasing per conversation).
    fn append_entry<'a>(
        &'a self,
        id: &'a str,
        entry: &'a Entry,
    ) -> BoxFuture<'a, Result<u64, AgentError>>;
    fn entries<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<(u64, Entry)>, AgentError>>;
    fn settings(&self) -> BoxFuture<'_, Result<AgentSettings, AgentError>>;
    fn save_settings<'a>(
        &'a self,
        settings: &'a AgentSettings,
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

/// Model access: catalog, accounts, streaming and judgments.
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
}
