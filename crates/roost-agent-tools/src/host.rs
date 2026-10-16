//! Per-conversation owner for worker-side tool state and dispatch.
//! The worker runtime calls `call`; its state map keeps hashline snapshots isolated.
//! Idle conversation state is discarded after an hour or explicit close.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use roost_protocol::wire::agent_chat::{
    BashArgs, EditArgs, GlobArgs, GrepArgs, JobArgs, LspArgs, ReadArgs, RewindFilesArgs, TOOL_BASH,
    TOOL_CONTEXT_FILES, TOOL_EDIT, TOOL_GLOB, TOOL_GREP, TOOL_JOB, TOOL_LSP, TOOL_READ,
    TOOL_REWIND_FILES, TOOL_WRITE, WriteArgs,
};
use tokio::sync::{Mutex, Notify, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    bash::{BashCall, DEFAULT_AUTO_BACKGROUND_AFTER, bash_tool},
    context_files::context_files_tool,
    file_tools,
    glob::glob_tool,
    grep::grep_tool,
    hashline::EditStore,
    jobs::{JobRegistry, SettledJob},
    lsp::LspManager,
    outcome::ToolOutcome,
    restore_points,
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

/// Removes a call's detach handle once the call returns, so a steer that arrives
/// after completion finds nothing to detach.
struct DetachableCall<'a> {
    host: &'a ToolHost,
    call_id: String,
}

impl Drop for DetachableCall<'_> {
    fn drop(&mut self) {
        self.host
            .detachable_calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.call_id);
    }
}

#[derive(Debug)]
pub struct ToolCallRequest {
    pub call_id: String,
    pub conversation_id: String,
    pub cwd: PathBuf,
    pub tool: String,
    pub args_json: String,
    pub timeout_ms: u32,
    pub checkpoint_id: u64,
}

pub struct ToolHost {
    data_dir: PathBuf,
    conversations: Mutex<HashMap<String, Arc<ConversationTools>>>,
    lsp: LspManager,
    jobs: Arc<JobRegistry>,
    auto_background_after: Duration,
    /// Running `bash` calls keyed by call id, so a steer detaches the right one.
    detachable_calls: std::sync::Mutex<HashMap<String, Arc<Notify>>>,
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
    pub fn new(data_dir: PathBuf, settled_jobs: mpsc::UnboundedSender<SettledJob>) -> Self {
        if let Err(error) = restore_points::prune_expired(&data_dir) {
            tracing::warn!(%error, "agent tool restore-point pruning failed");
        }
        Self {
            lsp: LspManager::new(data_dir.clone()),
            data_dir,
            conversations: Mutex::new(HashMap::new()),
            jobs: Arc::new(JobRegistry::new(settled_jobs)),
            auto_background_after: DEFAULT_AUTO_BACKGROUND_AFTER,
            detachable_calls: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Overrides how long a foreground `bash` call runs before it auto-backgrounds.
    pub fn with_auto_background_after(mut self, after: Duration) -> Self {
        self.auto_background_after = after;
        self
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
                        Some(file_tools::RestorePointContext {
                            data_dir: &self.data_dir,
                            conversation_id: &request.conversation_id,
                            checkpoint_id: request.checkpoint_id,
                        }),
                    )
                    .await
                }
                Err(error) => ToolOutcome::failure(error),
            },
            TOOL_EDIT => match decode::<EditArgs>(&request.args_json) {
                Ok(args) => {
                    file_tools::edit_tool_with_restore(
                        &mut conversation.state.lock().await.edit_store,
                        &request.cwd,
                        args,
                        &self.lsp,
                        &out,
                        Some(file_tools::RestorePointContext {
                            data_dir: &self.data_dir,
                            conversation_id: &request.conversation_id,
                            checkpoint_id: request.checkpoint_id,
                        }),
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
                    let detach = Arc::new(Notify::new());
                    let _registration = self.register_detachable(&request.call_id, detach.clone());
                    bash_tool(
                        BashCall {
                            data_dir: self.data_dir.clone(),
                            cwd: request.cwd,
                            conversation_id: request.conversation_id,
                            args,
                            timeout_ms: request.timeout_ms,
                            auto_background_after: self.auto_background_after,
                            detach,
                            jobs: self.jobs.clone(),
                        },
                        &out,
                        cancel,
                    )
                    .await
                }
                Err(error) => ToolOutcome::failure(error),
            },
            TOOL_JOB => match decode::<JobArgs>(&request.args_json) {
                Ok(args) => self.jobs.run_job_tool(&request.conversation_id, args).await,
                Err(error) => ToolOutcome::failure(error),
            },
            TOOL_LSP => match decode::<LspArgs>(&request.args_json) {
                Ok(args) => self.lsp.run_tool(&request.cwd, args, &out, cancel).await,
                Err(error) => ToolOutcome::failure(error),
            },
            TOOL_REWIND_FILES => match decode::<RewindFilesArgs>(&request.args_json) {
                Ok(args) => match restore_points::restore_from(
                    &self.data_dir,
                    &request.conversation_id,
                    args.checkpoint_id,
                ) {
                    Ok(report) => {
                        let mut sections = Vec::new();
                        if !report.restored.is_empty() {
                            sections.push(format!("Restored:\n{}", report.restored.join("\n")));
                        }
                        if !report.deleted.is_empty() {
                            sections.push(format!("Deleted:\n{}", report.deleted.join("\n")));
                        }
                        ToolOutcome::success(if sections.is_empty() {
                            "No files needed restoring.".to_owned()
                        } else {
                            sections.join("\n")
                        })
                    }
                    Err(error) => ToolOutcome::failure(format!("Unable to restore files: {error}")),
                },
                Err(error) => ToolOutcome::failure(error),
            },
            name => ToolOutcome::failure(format!("unknown tool: {name}")),
        };
        conversation.state.lock().await.last_used = Instant::now();
        drop(active_call);
        outcome
    }

    fn register_detachable(&self, call_id: &str, detach: Arc<Notify>) -> DetachableCall<'_> {
        self.detachable_calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(call_id.to_owned(), detach);
        DetachableCall {
            host: self,
            call_id: call_id.to_owned(),
        }
    }

    /// Detaches the running `bash` call with this id into a background job. Returns
    /// false when no such call is running.
    pub fn background_call(&self, call_id: &str) -> bool {
        let calls = self
            .detachable_calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match calls.get(call_id) {
            Some(detach) => {
                detach.notify_one();
                true
            }
            None => false,
        }
    }

    /// Ends the conversation's running background jobs; a turn abort takes them with it.
    pub fn abort_jobs(&self, conversation_id: &str) {
        self.jobs.cancel_running(conversation_id);
    }

    /// Ends every background job this worker runs; used when its coordinator link drops.
    pub fn cancel_all_jobs(&self) {
        self.jobs.cancel_all();
    }

    pub async fn close_conversation(&self, conversation_id: &str) {
        self.conversations.lock().await.remove(conversation_id);
        self.jobs.close_conversation(conversation_id);
        if let Err(error) = restore_points::close_conversation(&self.data_dir, conversation_id) {
            tracing::warn!(conversation_id, %error, "agent tool restore-point cleanup failed");
        }
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
        self.jobs.evict_idle();
    }
}

fn decode<T: serde::de::DeserializeOwned>(json: &str) -> Result<T, String> {
    serde_json::from_str(json).map_err(|error| format!("invalid tool arguments: {error}"))
}
