//! Per-conversation owner for worker-side tool state and dispatch.
//! The worker runtime calls `call`; its state map keeps hashline snapshots isolated.
//! Idle conversation state is discarded after an hour or explicit close.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use roost_protocol::wire::agent_chat::{
    BashArgs, EditArgs, GlobArgs, GrepArgs, LspArgs, ReadArgs, TOOL_BASH, TOOL_CONTEXT_FILES,
    TOOL_EDIT, TOOL_GLOB, TOOL_GREP, TOOL_LSP, TOOL_READ, TOOL_WRITE, WriteArgs,
};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    bash::bash_tool, context_files::context_files_tool, file_tools, glob::glob_tool,
    grep::grep_tool, hashline::EditStore, lsp::LspManager, outcome::ToolOutcome,
};

const IDLE_TTL: Duration = Duration::from_secs(60 * 60);

struct ConversationState {
    edit_store: EditStore,
    last_used: Instant,
}
struct ConversationTools {
    state: Mutex<ConversationState>,
    active_calls: AtomicUsize,
}
struct ActiveCall(Arc<ConversationTools>);

impl Drop for ActiveCall {
    fn drop(&mut self) {
        self.0.active_calls.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub struct ToolCallRequest {
    pub conversation_id: String,
    pub cwd: PathBuf,
    pub tool: String,
    pub args_json: String,
    pub timeout_ms: u32,
}

pub struct ToolHost {
    data_dir: PathBuf,
    conversations: Mutex<HashMap<String, Arc<ConversationTools>>>,
    lsp: LspManager,
}

impl std::fmt::Debug for ToolHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolHost")
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

impl ToolHost {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            lsp: LspManager::new(data_dir.clone()),
            data_dir,
            conversations: Mutex::new(HashMap::new()),
        }
    }

    pub async fn call(
        &self,
        request: ToolCallRequest,
        out: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> ToolOutcome {
        if cancel.is_cancelled() {
            return ToolOutcome::failure("tool call cancelled");
        }
        let (conversation, active_call) = {
            let mut conversations = self.conversations.lock().await;
            let conversation = conversations
                .entry(request.conversation_id.clone())
                .or_insert_with(|| {
                    Arc::new(ConversationTools {
                        state: Mutex::new(ConversationState {
                            edit_store: EditStore::default(),
                            last_used: Instant::now(),
                        }),
                        active_calls: AtomicUsize::new(0),
                    })
                })
                .clone();
            conversation.active_calls.fetch_add(1, Ordering::Relaxed);
            (Arc::clone(&conversation), ActiveCall(conversation))
        };
        conversation.state.lock().await.last_used = Instant::now();
        let outcome = match request.tool.as_str() {
            TOOL_READ => match decode::<ReadArgs>(&request.args_json) {
                Ok(args) => file_tools::read_tool(
                    &mut conversation.state.lock().await.edit_store,
                    &request.cwd,
                    args,
                ),
                Err(error) => ToolOutcome::failure(error),
            },
            TOOL_WRITE => match decode::<WriteArgs>(&request.args_json) {
                Ok(args) => {
                    file_tools::write_tool(
                        &mut conversation.state.lock().await.edit_store,
                        &request.cwd,
                        args,
                        &self.lsp,
                        &out,
                    )
                    .await
                }
                Err(error) => ToolOutcome::failure(error),
            },
            TOOL_EDIT => match decode::<EditArgs>(&request.args_json) {
                Ok(args) => {
                    file_tools::edit_tool(
                        &mut conversation.state.lock().await.edit_store,
                        &request.cwd,
                        args,
                        &self.lsp,
                        &out,
                    )
                    .await
                }
                Err(error) => ToolOutcome::failure(error),
            },
            TOOL_GREP => match decode::<GrepArgs>(&request.args_json) {
                Ok(args) => {
                    grep_tool(
                        &mut conversation.state.lock().await.edit_store,
                        &request.cwd,
                        args,
                    )
                    .await
                }
                Err(error) => ToolOutcome::failure(error),
            },
            TOOL_GLOB => match decode::<GlobArgs>(&request.args_json) {
                Ok(args) => glob_tool(&request.cwd, args).await,
                Err(error) => ToolOutcome::failure(error),
            },
            TOOL_CONTEXT_FILES => context_files_tool(&request.cwd).await,
            TOOL_BASH => match decode::<BashArgs>(&request.args_json) {
                Ok(args) => {
                    bash_tool(
                        &self.data_dir,
                        &request.cwd,
                        args,
                        request.timeout_ms,
                        &out,
                        cancel,
                    )
                    .await
                }
                Err(error) => ToolOutcome::failure(error),
            },
            TOOL_LSP => match decode::<LspArgs>(&request.args_json) {
                Ok(args) => self.lsp.run_tool(&request.cwd, args, &out, cancel).await,
                Err(error) => ToolOutcome::failure(error),
            },
            name => ToolOutcome::failure(format!("unknown tool: {name}")),
        };
        conversation.state.lock().await.last_used = Instant::now();
        drop(active_call);
        outcome
    }

    pub async fn close_conversation(&self, conversation_id: &str) {
        self.conversations.lock().await.remove(conversation_id);
    }

    pub async fn evict_idle(&self) {
        self.conversations.lock().await.retain(|_, conversation| {
            if conversation.active_calls.load(Ordering::Relaxed) > 0 {
                return true;
            }
            match conversation.state.try_lock() {
                Ok(tools) => tools.last_used.elapsed() < IDLE_TTL,
                Err(_) => true,
            }
        });
    }
}

fn decode<T: serde::de::DeserializeOwned>(json: &str) -> Result<T, String> {
    serde_json::from_str(json).map_err(|error| format!("invalid tool arguments: {error}"))
}
