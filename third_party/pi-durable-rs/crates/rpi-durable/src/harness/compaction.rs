//! Built-in compaction task (port of
//! `packages/durable/src/harness/compaction.ts` @ `abe508e1b`).
//!
//! A compaction selects an old prefix of the model context, summarizes it with a
//! separate model request, and appends a `pi.compaction` entry whose `head` is
//! the first kept entry. Generation owns blocking compactions (threshold and
//! overflow) and waits for them; [`SchedulerService::compact`] creates a
//! conversation-owned manual one.
//!
//! Conversation-owned summaries enter as deduplicated write submissions in
//! the same commit that completes their task; task-owned blocking summaries
//! append directly.
//!
//! [`SchedulerService::compact`]: crate::harness::scheduler::SchedulerService::compact

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use rpi_ai::models::ModelsSimpleStreamOptions;
use rpi_ai::types::{
    AssistantContent, AssistantMessage, CacheRetention, Context as ModelContext, Message,
    ModelThinkingLevel, ProviderRequestOptions, RequestOrigin, RequestOriginKind,
    SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel, UserContent, UserMessage,
    UserRole,
};
use rpi_ai::utils::estimate::{calculate_context_tokens, estimate_message_tokens};
use rpi_ai::utils::retry::{is_retryable_assistant_error, retry_delay_ms};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::chord::context::Context;
use crate::chord::json::JsonValue;
use crate::entries::COMPACTION_ENTRY;
use crate::harness::agent::agent_hooks;
use crate::harness::context::order_tool_results;
use crate::harness::live::{
    CompactionStatus, LIVE_DOC, RetryStatus, add_compaction_status, compaction_status,
    remove_compaction_status,
};
use crate::harness::registry::COMPACTION_TASK_NAME;
use crate::harness::types::{
    BeforeCompact, BeforeCompactResult, CompactionReason, CompactionResult,
    ConversationStreamOptions, HookHandlers, ModelRef, NextTaskState, RunningTask, TaskCommitFn,
    TaskDefinition, TaskRuntime, TaskWriteCommitFn, WriteSubmissionDraft,
};
use crate::harness::usage::{UsageBucket, record_usage};
use crate::ids::{ConversationId, EntryId, TaskId};
use crate::session::{
    DocDraft, DocOwner, EntryDraft, EntryHead, SessionError, SessionResult, TaskCreate,
    TaskOwnership, Transaction,
};
use crate::types::{TaskOutcome, TaskOutcomeError};

/// Longest tool result text a serialized summary source keeps.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

const SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
const SUMMARY_SUFFIX: &str = "\n</summary>";

const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work. If the conversation starts with an earlier summary, preserve its information and fold the newer messages into it.\n\nUse this EXACT format:\n\n## Goal\n[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned by user]\n- [Or \"(none)\" if none were mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Current work]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [Ordered list of what should happen next]\n\n## Critical Context\n- [Any data, examples, or references needed to continue]\n- [Or \"(none)\" if not applicable]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

/// Compaction input (`reason` and optional manual instructions).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionInput {
    /// Why the compaction runs.
    pub reason: CompactionReason,
    /// Manual focus instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// The pinned summarization request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SummaryRequest {
    attempt: u64,
    model: ModelRef,
    thinking_level: ModelThinkingLevel,
    stream_options: ConversationStreamOptions,
    max_tokens: u64,
    /// Newest entry of the context the range was selected from.
    tail: EntryId,
    /// First entry kept verbatim; the summary's `head`.
    first_kept: EntryId,
}

/// Durable compaction checkpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum CompactionCheckpoint {
    /// Range selection.
    #[serde(rename = "select")]
    Select,
    /// Summarization request.
    #[serde(rename = "summarize")]
    Summarize {
        /// Pinned request.
        #[serde(flatten)]
        request: SummaryRequest,
    },
    /// Retry backoff.
    #[serde(rename = "retry")]
    Retry {
        /// Pinned request.
        #[serde(flatten)]
        request: SummaryRequest,
        /// Wall-clock time of the next attempt.
        until: f64,
    },
}

/// Built-in compaction task (`pi.compaction`).
pub struct CompactionTask;

#[async_trait]
impl TaskDefinition for CompactionTask {
    fn name(&self) -> &str {
        COMPACTION_TASK_NAME
    }

    fn version(&self) -> u64 {
        1
    }

    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "select" })
    }

    async fn run_phase(
        &self,
        phase: &str,
        task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        let checkpoint: CompactionCheckpoint = parse_checkpoint(task.checkpoint())?;
        match phase {
            "select" => match checkpoint {
                CompactionCheckpoint::Select => select_phase(&task, runtime, context).await,
                _ => Err(SessionError::message(
                    "Compaction phase/checkpoint mismatch",
                )),
            },
            "summarize" => match checkpoint {
                CompactionCheckpoint::Summarize { request } => {
                    summarize_phase(&task, runtime, context, request).await
                }
                _ => Err(SessionError::message(
                    "Compaction phase/checkpoint mismatch",
                )),
            },
            "retry" => match checkpoint {
                CompactionCheckpoint::Retry { request, until } => {
                    retry_phase(runtime, context, request, until).await
                }
                _ => Err(SessionError::message(
                    "Compaction phase/checkpoint mismatch",
                )),
            },
            _ => Err(SessionError::message(format!(
                "Unknown compaction phase or mismatched checkpoint: {phase}"
            ))),
        }
    }

    async fn abort(
        &self,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        let conversation_id = runtime.conversation_id();
        let task_id = runtime.task_id();
        let change: TaskCommitFn = Arc::new(move |tx, _current| {
            Box::pin(async move {
                let live = tx
                    .doc(&LIVE_DOC, DocOwner::Conversation(conversation_id), None)
                    .await?;
                remove_compaction_status(&live, task_id)?;
                Ok(Some(NextTaskState::Terminal {
                    outcome: TaskOutcome::Aborted {
                        reason: Some("aborted".into()),
                        result: None,
                    },
                }))
            })
        });
        runtime.commit(change, context).await
    }
}

fn parse_checkpoint(checkpoint: Option<&JsonValue>) -> SessionResult<CompactionCheckpoint> {
    match checkpoint {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| SessionError::message(error.to_string())),
        None => Ok(CompactionCheckpoint::Select),
    }
}

fn input_of(task: &RunningTask) -> SessionResult<CompactionInput> {
    serde_json::from_value(task.record.input.clone())
        .map_err(|error| SessionError::message(format!("Invalid compaction input: {error}")))
}

async fn fail_no_model(
    runtime: &dyn TaskRuntime,
    reference: Option<&ModelRef>,
    context: &Context,
) -> SessionResult<()> {
    let message = match reference {
        Some(reference) => format!(
            "Model {}/{} is not available",
            reference.provider, reference.model_id
        ),
        None => "No model is configured".to_string(),
    };
    let conversation_id = runtime.conversation_id();
    let task_id = runtime.task_id();
    let change: TaskCommitFn = Arc::new(move |tx, _current| {
        let message = message.clone();
        Box::pin(async move {
            let live = tx
                .doc(&LIVE_DOC, DocOwner::Conversation(conversation_id), None)
                .await?;
            remove_compaction_status(&live, task_id)?;
            Ok(Some(NextTaskState::Terminal {
                outcome: TaskOutcome::Failed {
                    error: TaskOutcomeError {
                        message,
                        detail: Some(serde_json::json!({ "reason": "no_model" })),
                    },
                    result: None,
                },
            }))
        })
    });
    runtime.commit(change, context).await
}

async fn complete_without_summary(
    runtime: &dyn TaskRuntime,
    context: &Context,
) -> SessionResult<()> {
    let conversation_id = runtime.conversation_id();
    let task_id = runtime.task_id();
    let change: TaskCommitFn = Arc::new(move |tx, _current| {
        Box::pin(async move {
            let live = tx
                .doc(&LIVE_DOC, DocOwner::Conversation(conversation_id), None)
                .await?;
            remove_compaction_status(&live, task_id)?;
            Ok(Some(NextTaskState::Terminal {
                outcome: TaskOutcome::Completed {
                    result: serde_json::json!({}),
                },
            }))
        })
    });
    runtime.commit(change, context).await
}

async fn select_phase(
    task: &RunningTask,
    runtime: &dyn TaskRuntime,
    context: &Context,
) -> SessionResult<()> {
    let agent = runtime.agent(context).await?;
    let settings = runtime.settings();
    let Some(reference) = agent.model.clone() else {
        return fail_no_model(runtime, None, context).await;
    };
    let Some(_model) = runtime
        .models()
        .get_model(&reference.provider, &reference.model_id)
    else {
        return fail_no_model(runtime, Some(&reference), context).await;
    };
    let policy = settings.compaction;
    let view = runtime
        .context(runtime.conversation_id(), context, None)
        .await?;
    let Some(cut) = select_cut(&view, policy.keep_recent_tokens) else {
        return complete_without_summary(runtime, context).await;
    };
    let first_kept = view.entries[cut].id;
    let input = input_of(task)?;
    let compaction = BeforeCompact {
        reason: input.reason,
        entries: view.entries[..cut].to_vec(),
        messages: summarized_messages(&view, cut),
        first_kept,
        instructions: input.instructions.clone(),
    };
    let mut decision: Option<BeforeCompactResult> = None;
    for handlers in agent_hooks(&agent, COMPACTION_TASK_NAME) {
        if decision.is_some() {
            break;
        }
        if let HookHandlers::Compaction(hooks) = handlers {
            let api = crate::harness::types::RuntimeHookApi { runtime };
            decision = hooks
                .before_compact(compaction.clone(), &api, context)
                .await;
        }
    }
    match decision {
        Some(BeforeCompactResult::Decline) => {
            return complete_without_summary(runtime, context).await;
        }
        Some(BeforeCompactResult::Summary(summary)) => {
            return place(task, runtime, first_kept, input.reason, summary, context).await;
        }
        None => {}
    }
    let tail = view
        .entries
        .iter()
        .map(|entry| entry.id)
        .max()
        .unwrap_or(first_kept);
    let mut summary_reference = reference.clone();
    for handlers in agent_hooks(&agent, COMPACTION_TASK_NAME) {
        if let HookHandlers::Compaction(hooks) = handlers {
            if let Some(selected) = hooks
                .select_model(summary_reference.clone(), context)
                .await?
            {
                summary_reference = selected;
                break;
            }
        }
    }
    let Some(summary_model) = runtime
        .models()
        .get_model(&summary_reference.provider, &summary_reference.model_id)
    else {
        return fail_no_model(runtime, Some(&summary_reference), context).await;
    };
    let request = SummaryRequest {
        attempt: 1,
        model: summary_reference,
        thinking_level: agent.thinking_level,
        stream_options: settings.stream,
        max_tokens: summary_max_tokens(policy.reserve_tokens, summary_model.max_tokens),
        tail,
        first_kept,
    };
    let checkpoint = serde_json::to_value(CompactionCheckpoint::Summarize { request })
        .map_err(|error| SessionError::message(error.to_string()))?;
    let change: TaskCommitFn = Arc::new(move |_tx, _current| {
        let checkpoint = checkpoint.clone();
        Box::pin(async move { Ok(Some(NextTaskState::Running { checkpoint })) })
    });
    runtime.commit(change, context).await
}

fn summary_max_tokens(reserve_tokens: u64, model_max_tokens: u32) -> u64 {
    let budget = (reserve_tokens as f64 * 0.8).floor() as u64;
    let model_cap = if model_max_tokens > 0 {
        model_max_tokens as u64
    } else {
        u64::MAX
    };
    budget.min(model_cap)
}

async fn summarize_phase(
    task: &RunningTask,
    runtime: &dyn TaskRuntime,
    context: &Context,
    request: SummaryRequest,
) -> SessionResult<()> {
    let model = runtime
        .models()
        .get_model(&request.model.provider, &request.model.model_id);
    let Some(model) = model else {
        return fail_no_model(runtime, Some(&request.model), context).await;
    };
    let view = runtime
        .context(runtime.conversation_id(), context, Some(request.tail))
        .await?;
    let Some(cut) = summary_boundary(&view, request.first_kept) else {
        return complete_without_summary(runtime, context).await;
    };
    let now = runtime.now()? as i64;
    let input = input_of(task)?;
    let messages: Vec<Message> = vec![
        Message::System(system_message(SUMMARIZATION_SYSTEM_PROMPT, now)),
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(summary_prompt(
                &summarized_messages(&view, cut),
                input.instructions.as_deref(),
            )),
            timestamp: now,
        }),
    ];
    let mut options = request_stream_options(
        &request.stream_options,
        runtime.signal(),
        runtime.provider_session_id(context).await?,
    );
    options.cache_retention = Some(CacheRetention::None);
    options.origin = Some(RequestOrigin {
        kind: RequestOriginKind::Compaction,
        id: runtime.task_id().as_number().to_string(),
    });
    options.max_tokens = Some(request.max_tokens.min(u32::MAX as u64) as u32);
    let simple = SimpleStreamOptions {
        stream: options,
        reasoning: reasoning_level(request.thinking_level),
        thinking_budgets: None,
        tool_choice: None,
    };
    let model_context = ModelContext {
        system_prompt: None,
        messages,
        tools: None,
    };
    let message = runtime
        .models()
        .complete_simple(
            &model,
            &model_context,
            Some(ModelsSimpleStreamOptions::from(simple)),
        )
        .await
        .ok_or_else(|| SessionError::message("Summarization produced no response"))?;
    if runtime.signal().is_cancelled() {
        return Err(SessionError::Aborted);
    }
    let summary = summary_text(&message);
    let policy = runtime.settings().retry;
    let retry = message.stop_reason == StopReason::Error
        && is_retryable_assistant_error(&message)
        && policy.enabled
        && request.attempt <= policy.max_retries as u64;
    let until = if retry {
        runtime.now()?
            + retry_delay_ms(
                policy.base_delay_ms,
                policy.max_agent_delay_ms,
                request.attempt as u32,
            ) as f64
    } else {
        0.0
    };
    let conversation_id = runtime.conversation_id();
    let task_id = runtime.task_id();
    let reason = input.reason;
    if let Some(summary_text) = summary.clone() {
        if task.record.owner.is_none() {
            let write = WriteSubmissionDraft {
                request_id: Some(format!("compaction:{task_id}")),
                entry: summary_entry(request.first_kept, reason, summary_text, message.timestamp),
            };
            let recorded_message = message.clone();
            let change: TaskWriteCommitFn = Arc::new(move |tx, _current, submission_id| {
                let message = recorded_message.clone();
                Box::pin(async move {
                    record_usage(
                        tx,
                        conversation_id,
                        UsageBucket::Models,
                        &format!("{}/{}", message.provider, message.model),
                        &message.usage,
                    )
                    .await?;
                    let live = tx
                        .doc(&LIVE_DOC, DocOwner::Conversation(conversation_id), None)
                        .await?;
                    remove_compaction_status(&live, task_id)?;
                    let result = serde_json::to_value(CompactionResult {
                        entry_id: None,
                        submission_id: Some(submission_id),
                    })
                    .map_err(|error| SessionError::message(error.to_string()))?;
                    Ok(Some(NextTaskState::Terminal {
                        outcome: TaskOutcome::Completed { result },
                    }))
                })
            });
            runtime.commit_with_write(write, change, context).await?;
            return Ok(());
        }
    }
    let change: TaskCommitFn = Arc::new(move |tx, _current| {
        let message = message.clone();
        let request = request.clone();
        let summary = summary.clone();
        Box::pin(async move {
            record_usage(
                tx,
                conversation_id,
                UsageBucket::Models,
                &format!("{}/{}", message.provider, message.model),
                &message.usage,
            )
            .await?;
            let live = tx
                .doc(&LIVE_DOC, DocOwner::Conversation(conversation_id), None)
                .await?;
            if let Some(summary) = summary {
                return place_summary(
                    tx,
                    &live,
                    task_id,
                    conversation_id,
                    summary_entry(request.first_kept, reason, summary, message.timestamp),
                )
                .await;
            }
            if retry {
                if let Some(mut status) = compaction_status(&live, task_id)? {
                    status.retry = Some(RetryStatus {
                        at: until,
                        error: message.error_message.clone().unwrap_or_default(),
                    });
                    write_compaction_status(&live, status)?;
                }
                let checkpoint =
                    serde_json::to_value(CompactionCheckpoint::Retry { request, until })
                        .map_err(|error| SessionError::message(error.to_string()))?;
                return Ok(Some(NextTaskState::Sleeping { checkpoint, until }));
            }
            remove_compaction_status(&live, task_id)?;
            let text = summary_failure(&message);
            Ok(Some(NextTaskState::Terminal {
                outcome: TaskOutcome::Failed {
                    error: TaskOutcomeError {
                        message: text,
                        detail: Some(serde_json::json!({ "reason": "model_error" })),
                    },
                    result: None,
                },
            }))
        })
    });
    runtime.commit(change, context).await
}

async fn retry_phase(
    runtime: &dyn TaskRuntime,
    context: &Context,
    request: SummaryRequest,
    until: f64,
) -> SessionResult<()> {
    if until > runtime.now()? {
        let checkpoint = serde_json::to_value(CompactionCheckpoint::Retry {
            request: request.clone(),
            until,
        })
        .map_err(|error| SessionError::message(error.to_string()))?;
        runtime.sleep_until(until, checkpoint, context).await?;
        return Ok(());
    }
    let attempt = request.attempt + 1;
    let conversation_id = runtime.conversation_id();
    let task_id = runtime.task_id();
    let change: TaskCommitFn = Arc::new(move |tx, _current| {
        let request = request.clone();
        Box::pin(async move {
            let live = tx
                .doc(&LIVE_DOC, DocOwner::Conversation(conversation_id), None)
                .await?;
            if let Some(mut status) = compaction_status(&live, task_id)? {
                status.attempt = attempt;
                status.retry = None;
                write_compaction_status(&live, status)?;
            }
            let next = SummaryRequest { attempt, ..request };
            let checkpoint =
                serde_json::to_value(CompactionCheckpoint::Summarize { request: next })
                    .map_err(|error| SessionError::message(error.to_string()))?;
            Ok(Some(NextTaskState::Running { checkpoint }))
        })
    });
    runtime.commit(change, context).await
}

/// Place a summary supplied by a hook in its own commit.
async fn place(
    task: &RunningTask,
    runtime: &dyn TaskRuntime,
    first_kept: EntryId,
    reason: CompactionReason,
    summary: String,
    context: &Context,
) -> SessionResult<()> {
    let conversation_id = runtime.conversation_id();
    let task_id = runtime.task_id();
    let timestamp = runtime.now()? as i64;
    let draft = summary_entry(first_kept, reason, summary, timestamp);
    if task.record.owner.is_none() {
        let write = WriteSubmissionDraft {
            request_id: Some(format!("compaction:{task_id}")),
            entry: draft,
        };
        let change: TaskWriteCommitFn = Arc::new(move |tx, _current, submission_id| {
            Box::pin(async move {
                let live = tx
                    .doc(&LIVE_DOC, DocOwner::Conversation(conversation_id), None)
                    .await?;
                remove_compaction_status(&live, task_id)?;
                let result = serde_json::to_value(CompactionResult {
                    entry_id: None,
                    submission_id: Some(submission_id),
                })
                .map_err(|error| SessionError::message(error.to_string()))?;
                Ok(Some(NextTaskState::Terminal {
                    outcome: TaskOutcome::Completed { result },
                }))
            })
        });
        runtime.commit_with_write(write, change, context).await?;
        return Ok(());
    }
    let change: TaskCommitFn = Arc::new(move |tx, _current| {
        let draft = draft.clone();
        Box::pin(async move {
            let live = tx
                .doc(&LIVE_DOC, DocOwner::Conversation(conversation_id), None)
                .await?;
            place_summary(tx, &live, task_id, conversation_id, draft).await
        })
    });
    runtime.commit(change, context).await
}

fn summary_entry(
    first_kept: EntryId,
    reason: CompactionReason,
    summary: String,
    timestamp: i64,
) -> EntryDraft {
    let text = format!("{SUMMARY_PREFIX}{summary}{SUMMARY_SUFFIX}");
    let mut draft = EntryDraft::new(COMPACTION_ENTRY);
    draft.head = Some(EntryHead::At(first_kept));
    draft.model = Some(vec![Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text),
        timestamp,
    })]);
    draft.data = Some(serde_json::json!({ "reason": reason_text(reason) }));
    draft
}

/// Remove the status, append the blocking summary, and complete with its entry ID.
async fn place_summary(
    tx: &mut Transaction,
    live: &DocDraft,
    task_id: TaskId,
    conversation_id: ConversationId,
    draft: EntryDraft,
) -> SessionResult<Option<NextTaskState>> {
    remove_compaction_status(live, task_id)?;
    let record = tx.append_entry(conversation_id, draft).await?;
    let result = serde_json::to_value(CompactionResult {
        entry_id: Some(record.id),
        submission_id: None,
    })
    .map_err(|error| SessionError::message(error.to_string()))?;
    Ok(Some(NextTaskState::Terminal {
        outcome: TaskOutcome::Completed { result },
    }))
}

fn write_compaction_status(live: &DocDraft, status: CompactionStatus) -> SessionResult<()> {
    // Replace in place: the list stays sorted by task ID, and the task ID is
    // unchanged, so no re-ordering is needed.
    let mut state: crate::harness::live::LiveState = crate::harness::types::read_state(live)?;
    if let Some(list) = state.compactions.as_mut()
        && let Some(slot) = list.iter_mut().find(|slot| slot.task_id == status.task_id)
    {
        *slot = status;
    }
    crate::harness::types::write_state(live, &state)
}

fn reason_text(reason: CompactionReason) -> &'static str {
    match reason {
        CompactionReason::Manual => "manual",
        CompactionReason::Threshold => "threshold",
        CompactionReason::Overflow => "overflow",
    }
}

fn system_message(text: &str, timestamp: i64) -> rpi_ai::types::SystemMessage {
    rpi_ai::types::SystemMessage {
        role: rpi_ai::types::SystemRole::System,
        content: rpi_ai::types::SystemContent::Text(text.to_string()),
        sections: None,
        tools_added: None,
        tools_removed: None,
        timestamp,
    }
}

/// Create a compaction task with its status in this commit. `owner` is the
/// generation that waits for it (a blocking compaction); without one it is
/// conversation-owned, and `background` unless it is manual.
pub async fn create_compaction(
    tx: &mut Transaction,
    conversation_id: ConversationId,
    reason: CompactionReason,
    instructions: Option<String>,
    owner: Option<TaskId>,
) -> SessionResult<TaskId> {
    let ownership = match owner {
        Some(owner) => TaskOwnership::Task(owner),
        None => TaskOwnership::Conversation(conversation_id),
    };
    let background = owner.is_none() && reason != CompactionReason::Manual;
    let input = CompactionInput {
        reason,
        instructions,
    };
    let task_id = tx
        .create_task(TaskCreate {
            kind: COMPACTION_TASK_NAME.to_string(),
            version: 1,
            input: serde_json::to_value(input).unwrap_or(JsonValue::Null),
            checkpoint: serde_json::json!({ "phase": "select" }),
            ownership,
            conversation_id: Some(conversation_id),
            background,
        })
        .await?;
    let status = CompactionStatus {
        task_id,
        reason,
        blocking: owner.is_some(),
        attempt: 1,
        retry: None,
    };
    let live = tx
        .doc(&LIVE_DOC, DocOwner::Conversation(conversation_id), None)
        .await?;
    add_compaction_status(&live, status)?;
    Ok(task_id)
}

/// Locate the pinned summary boundary in the transcript view at its cutoff.
fn summary_boundary(
    view: &crate::harness::types::ContextView,
    first_kept: EntryId,
) -> Option<usize> {
    view.entries.iter().position(|entry| entry.id == first_kept)
}

/// Index in `view.entries` of the first entry a summary keeps, or `None` when
/// there is nothing to compact.
pub fn select_cut(
    view: &crate::harness::types::ContextView,
    keep_recent_tokens: u64,
) -> Option<usize> {
    let contributions = &view.contributions;
    let start = if view.head.is_some() { 1 } else { 0 };
    let mut candidates: Vec<usize> = Vec::new();
    for index in start..contributions.len() {
        if is_candidate(contributions, index) {
            candidates.push(index);
        }
    }
    let mut kept = 0u64;
    let mut cut: Option<usize> = None;
    for index in (start..contributions.len()).rev() {
        for message in &contributions[index] {
            kept += estimate_message_tokens(message);
        }
        if kept < keep_recent_tokens {
            continue;
        }
        cut = candidates
            .iter()
            .copied()
            .find(|candidate| *candidate >= index)
            .or_else(|| candidates.last().copied());
        break;
    }
    let cut = cut?;
    if contributions[start..cut]
        .iter()
        .any(|entry| !entry.is_empty())
    {
        return Some(cut);
    }
    None
}

fn is_candidate(contributions: &[Vec<Message>], index: usize) -> bool {
    let Some(first) = contributions[index].first() else {
        return false;
    };
    if first.role() == rpi_ai::types::Role::Assistant {
        return true;
    }
    if first.role() != rpi_ai::types::Role::User {
        return false;
    }
    let mut calls: BTreeSet<String> = BTreeSet::new();
    let mut before = index;
    while before > 0 {
        before -= 1;
        if let Some(Message::Assistant(assistant)) = contributions[before]
            .iter()
            .rev()
            .find(|message| message.role() == rpi_ai::types::Role::Assistant)
        {
            calls = assistant
                .content
                .iter()
                .filter_map(|content| match content {
                    AssistantContent::ToolCall(call) => Some(call.id.clone()),
                    _ => None,
                })
                .collect();
            break;
        }
    }
    if calls.is_empty() {
        return true;
    }
    for (position, message) in contributions[index..].iter().flatten().enumerate() {
        let absolute = index + position;
        if message.role() == rpi_ai::types::Role::Assistant && (absolute > index || position > 0) {
            return true;
        }
        if let Message::ToolResult(result) = message
            && calls.contains(&result.tool_call_id)
        {
            return false;
        }
    }
    true
}

/// Model messages of the entries before `cut`, ordered like model context.
pub fn summarized_messages(view: &crate::harness::types::ContextView, cut: usize) -> Vec<Message> {
    let flat: Vec<Message> = view.contributions[..cut.min(view.contributions.len())]
        .iter()
        .flatten()
        .cloned()
        .collect();
    order_tool_results(&flat)
}

/// Size of a request over `view` followed by `extra`.
pub fn estimate_context(view: &crate::harness::types::ContextView, extra: &[Message]) -> u64 {
    let after = view.head.as_ref().map(|head| head.id.get());
    let mut measured: Option<&AssistantMessage> = None;
    for index in (0..view.entries.len()).rev() {
        let id = view.entries[index].id.get();
        if let Some(after) = after
            && id <= after
        {
            continue;
        }
        measured = view.contributions[index]
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Assistant(assistant) if calculate_context_tokens(&assistant.usage) > 0 => {
                    Some(assistant)
                }
                _ => None,
            });
        if measured.is_some() {
            break;
        }
    }
    let (from, mut tokens) = match measured {
        None => (0usize, 0u64),
        Some(assistant) => {
            let position = view
                .messages
                .iter()
                .position(
                    |message| matches!(message, Message::Assistant(other) if other == assistant),
                )
                .map(|position| position + 1)
                .unwrap_or(view.messages.len());
            (position, calculate_context_tokens(&assistant.usage))
        }
    };
    for message in view.messages.iter().skip(from) {
        tokens += estimate_message_tokens(message);
    }
    for message in extra {
        tokens += estimate_message_tokens(message);
    }
    tokens
}

/// The summary of a clean `stop` with text and no tool call; anything else is
/// not a summary.
fn summary_text(message: &AssistantMessage) -> Option<String> {
    if message.stop_reason != StopReason::Stop
        || message
            .content
            .iter()
            .any(|content| matches!(content, AssistantContent::ToolCall(_)))
    {
        return None;
    }
    let text = message
        .content
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn summary_failure(message: &AssistantMessage) -> String {
    match message.stop_reason {
        StopReason::Error | StopReason::Aborted => format!(
            "Summarization failed: {}",
            message
                .error_message
                .clone()
                .unwrap_or_else(|| format!("{:?}", message.stop_reason))
        ),
        StopReason::Length => {
            "Summarization hit the token limit; the summary is incomplete".to_string()
        }
        _ if message
            .content
            .iter()
            .any(|content| matches!(content, AssistantContent::ToolCall(_))) =>
        {
            "Summarization attempted to call a tool".to_string()
        }
        _ => "Summarization produced no text".to_string(),
    }
}

fn summary_prompt(messages: &[Message], instructions: Option<&str>) -> String {
    let focus = instructions
        .map(|instructions| format!("\n\nAdditional focus: {instructions}"))
        .unwrap_or_default();
    format!(
        "<conversation>\n{}\n</conversation>\n\n{SUMMARIZATION_PROMPT}{focus}",
        serialize_conversation(messages)
    )
}

/// Messages as plain text, so the summarizer reads a transcript instead of
/// continuing it. System messages are omitted.
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for message in messages {
        match message {
            Message::User(user) => {
                let text = content_text(&user.content);
                if !text.is_empty() {
                    parts.push(format!("[User]: {text}"));
                }
            }
            Message::Assistant(assistant) => {
                let thinking: Vec<String> = assistant
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        AssistantContent::Thinking(thinking) => Some(thinking.thinking.clone()),
                        _ => None,
                    })
                    .collect();
                let text: Vec<String> = assistant
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        AssistantContent::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect();
                let calls: Vec<String> = assistant
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        AssistantContent::ToolCall(call) => Some(format!(
                            "{}({})",
                            call.name,
                            call.arguments
                                .iter()
                                .map(|(key, value)| format!("{key}={value}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                        _ => None,
                    })
                    .collect();
                if !thinking.is_empty() {
                    parts.push(format!("[Assistant thinking]: {}", thinking.join("\n")));
                }
                if !text.is_empty() {
                    parts.push(format!("[Assistant]: {}", text.join("\n")));
                }
                if !calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", calls.join("; ")));
                }
            }
            Message::ToolResult(result) => {
                let text = tool_result_text(result);
                if !text.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate(&text, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            Message::System(_) => {}
        }
    }
    parts.join("\n\n")
}

fn content_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                rpi_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn tool_result_text(result: &rpi_ai::types::ToolResultMessage) -> String {
    result
        .content
        .iter()
        .filter_map(|content| match content {
            rpi_ai::types::ToolResultContent::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    let dropped = text.chars().count() - max_chars;
    format!("{kept}\n\n[... {dropped} more characters truncated]")
}

/// Map a resolved [`ConversationStreamOptions`] onto pi-ai request options,
/// carrying the invocation's cancellation token and the provider session ID.
pub(crate) fn request_stream_options(
    base: &ConversationStreamOptions,
    signal: CancellationToken,
    session_id: String,
) -> StreamOptions {
    StreamOptions {
        request: ProviderRequestOptions {
            signal: Some(signal),
            timeout_ms: base.timeout_ms,
            max_retries: base.max_retries,
            max_retry_delay_ms: base.max_retry_delay_ms,
            headers: base.headers.as_ref().map(|headers| {
                headers
                    .iter()
                    .map(|(name, value)| (name.clone(), Some(value.clone())))
                    .collect()
            }),
            ..Default::default()
        },
        transport: base.transport,
        cache_retention: base.cache_retention,
        session_id: Some(session_id),
        metadata: base.metadata.clone(),
        ..Default::default()
    }
}

/// Map an off-inclusive thinking level onto the simple-request reasoning level.
pub(crate) fn reasoning_level(level: ModelThinkingLevel) -> Option<ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpi_ai::types::{TextContent, Usage};

    fn assistant(text: &str, stop: StopReason) -> AssistantMessage {
        AssistantMessage {
            content: vec![AssistantContent::Text(TextContent {
                text: text.to_string(),
                text_signature: None,
            })],
            provider: "p".into(),
            model: "m".into(),
            usage: Usage::default(),
            stop_reason: stop,
            timestamp: 1,
            ..serde_json::from_value::<AssistantMessage>(serde_json::json!({
                "role": "assistant",
                "content": [],
                "api": "openai",
                "provider": "p",
                "model": "m",
                "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                    "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0 } },
                "stopReason": "stop",
                "timestamp": 1
            }))
            .expect("assistant")
        }
    }

    #[test]
    fn serialize_conversation_reads_roles() {
        let text = serialize_conversation(&[
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hi".into()),
                timestamp: 1,
            }),
            Message::Assistant(assistant("hello", StopReason::Stop)),
        ]);
        assert!(text.contains("[User]: hi"));
        assert!(text.contains("[Assistant]: hello"));
    }

    #[test]
    fn summary_text_requires_stop_and_text() {
        assert!(summary_text(&assistant("ok", StopReason::Stop)).is_some());
        assert!(summary_text(&assistant("", StopReason::Stop)).is_none());
        assert!(summary_text(&assistant("ok", StopReason::Length)).is_none());
    }

    #[test]
    fn stale_pinned_summary_boundary_is_not_summarized() {
        let view = crate::harness::types::ContextView {
            head: None,
            entries: Vec::new(),
            contributions: Vec::new(),
            messages: Vec::new(),
        };
        assert_eq!(summary_boundary(&view, EntryId::from_number(1)), None);
    }
}
