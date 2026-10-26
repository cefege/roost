//! Native recovery traces for interrupted, nested, observational, and progress behavior.
//! Dispatches to focused scenario owners using shared fixture and storage helpers.
//! Each producer drives the shipped durable kernel and captures its actual effects.
use crate::provider::{OracleProvider, message, tool_call};
use crate::scenarios::{entries, input, options, scratch_sqlite_path};
use async_trait::async_trait;
use rpi_ai::auth::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthResult, ModelAuth, ProviderAuth,
};
use rpi_ai::models::{Models, Provider};
use rpi_ai::types::{
    ApiKind, AssistantContent, DoneReason, InputModality, Model, ModelCost, SimpleStreamOptions,
    StopReason, StreamEvent, StreamOptions, TextContent, Tool, ToolResultContent,
    TranscriptContext,
};
use rpi_ai::utils::event_stream::AssistantMessageEventStream;
use rpi_durable::SessionError;
use rpi_durable::chord::context::{BACKGROUND_CONTEXT, Context};
use rpi_durable::chord::json::JsonObject;
use rpi_durable::harness::live::LIVE_DOC;
use rpi_durable::harness::{
    AgentChange, Extension, ExtensionsChange, HarnessOptions, ModelRef, Registry, RegistryImpl,
    ToolExecute, ToolExecuteError, ToolExecutionApi, ToolExecutionResult, ToolRegistration,
    ToolReplay, builtin_tasks, open_harness,
};
use rpi_durable::ids::{ConversationId, SubmissionId};
use rpi_durable::session::DocOwner;
use rpi_durable::storage::{
    SqliteDatabaseOptions, open_node_sqlite_database, open_node_sqlite_storage,
};
use rpi_durable::types::{DocumentAddress, DocumentPoint, DocumentScope, Storage};
use serde_json::{Value, json};
use std::error::Error;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[path = "deferred_progress.rs"]
mod deferred_progress;
#[path = "nested_recovery.rs"]
mod nested_recovery;
#[path = "read_only_observation.rs"]
mod read_only_observation;
#[path = "unsafe_recovery.rs"]
mod unsafe_recovery;

pub async fn run(name: &str) -> Result<Value, Box<dyn Error>> {
    match name {
        "unsafe-tool-sigkill-recovery" => unsafe_recovery::unsafe_tool_sigkill_recovery().await,
        "nested-tool-identity-replay-structured-error" => {
            nested_recovery::nested_tool_identity_replay_structured_error().await
        }
        "read-only-observation" => read_only_observation::read_only_observation().await,
        "deferred-document-progress" => deferred_progress::deferred_document_progress().await,
        _ => Err(format!("unknown recovery trace {name}").into()),
    }
}

fn cleanup_sqlite_path(path: &str) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{path}{suffix}"));
    }
}

struct ScratchDatabase(String);

impl Drop for ScratchDatabase {
    fn drop(&mut self) {
        cleanup_sqlite_path(&self.0);
    }
}
