//! One turn of a conversation: compaction check, model and thinking-level
//! resolution, request assembly, the streamed model call, then the tool round
//! its tool calls ask for. The run loop repeats turns until one ends the run.

use roost_llm::{ChatRequest, Message, ModelInfo};
use roost_protocol::wire::agent_chat::ModelRef;
use std::collections::VecDeque;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::model_call::{self, ModelOutput};
use crate::prompts::{self, SubagentPrompt};
use crate::records::{ConversationRecord, Entry, Mode, Role};
use crate::roles::{self, ResolvedModel};
use crate::runtime::{AgentRuntime, RunEnd, Steer, latest_submission_seq};
use crate::toolset::{SubagentKind, toolset_for};
use crate::{auto_thinking, compaction, context_files, find, history, tool_round};

/// Per-run state the loop carries between turns.
#[derive(Debug, Default)]
pub(crate) struct RunMemory {
    /// The `auto` thinking level chosen for the current user submission.
    pub auto_level: Option<&'static str>,
    /// Whether unexpected-stop detection already continued this submission.
    pub continued: bool,
    /// Yield reminders sent to a subagent that stopped without yielding.
    pub yield_reminders: u32,
    /// Accepted steers received during the goal idle window.
    pub pending_steers: VecDeque<Steer>,
}

impl RunMemory {
    pub fn new_submission(&mut self) {
        self.auto_level = None;
        self.continued = false;
    }
}

#[derive(Debug)]
pub(crate) enum TurnOutcome {
    /// The model answered without tool calls.
    Text {
        output: Box<ModelOutput>,
        model: Box<ResolvedModel>,
    },
    /// A tool round ran; the run continues unless it carries an end.
    Tools { end: Option<RunEnd> },
}

/// The model a turn calls first: the head of its candidate chain.
pub(crate) async fn conversation_model(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
) -> Result<ResolvedModel, AgentError> {
    conversation_chain(runtime, record)
        .await?
        .into_iter()
        .next()
        .ok_or_else(no_model_available)
}

/// Side questions and compaction use the scoped lightweight role when set;
/// otherwise they keep the conversation's model and ordered fallbacks.
pub(crate) async fn side_task_models(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
) -> Result<Vec<ModelInfo>, AgentError> {
    let settings = runtime.settings_for(record).await?;
    let configured = settings.model_roles.contains_key(&Role::Tiny)
        || settings.model_roles.contains_key(&Role::Smol);
    let chain = if configured {
        roles::role_chain(runtime.inner.llm.as_ref(), &settings, Role::Tiny).await
    } else {
        conversation_chain_with_settings(runtime, record, &settings).await?
    };
    if chain.is_empty() {
        return Err(no_model_available());
    }
    Ok(chain.into_iter().map(|candidate| candidate.info).collect())
}

/// The candidates a turn tries in order. In plan mode an explicit session model
/// answers alone, then the Planning chain, then the model saved when plan mode
/// began. A subagent's configured role chain beats the parent pin it inherited.
/// Otherwise an explicit session model answers alone, then the folder's
/// `default` chain. Chains come from the conversation's folder-merged settings.
pub(crate) async fn conversation_chain(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
) -> Result<Vec<ResolvedModel>, AgentError> {
    let settings = runtime.settings_for(record).await?;
    conversation_chain_with_settings(runtime, record, &settings).await
}

pub(crate) async fn conversation_chain_with_settings(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    settings: &crate::records::AgentSettings,
) -> Result<Vec<ResolvedModel>, AgentError> {
    let llm = runtime.inner.llm.as_ref();
    if record.mode == Mode::Plan {
        if let Some(reference) = &record.model {
            return Ok(vec![session_candidate(runtime, reference).await?]);
        }
        let chain = roles::role_chain(llm, settings, Role::Plan).await;
        if !chain.is_empty() {
            return Ok(chain);
        }
        if let Some(reference) = &record.pre_plan_model {
            return Ok(vec![session_candidate(runtime, reference).await?]);
        }
    } else if let Some(role) = subagent_role(record) {
        let chain = roles::role_chain(llm, settings, role).await;
        if !chain.is_empty() {
            return Ok(chain);
        }
    }
    if let Some(reference) = &record.model {
        return Ok(vec![session_candidate(runtime, reference).await?]);
    }
    let chain = roles::role_chain(llm, settings, Role::Default).await;
    if chain.is_empty() {
        return Err(no_model_available());
    }
    Ok(chain)
}

/// The role a subagent's turns draw from. Main and plan-mode turns have none.
fn subagent_role(record: &ConversationRecord) -> Option<Role> {
    let agent = record.agent.as_deref()?;
    roost_protocol::wire::agent_chat::subagent_role(agent).and_then(Role::from_name)
}

/// An explicit session model, checked against the catalog and credentials.
async fn session_candidate(
    runtime: &AgentRuntime,
    reference: &ModelRef,
) -> Result<ResolvedModel, AgentError> {
    let llm = runtime.inner.llm.as_ref();
    let info = llm
        .catalog()
        .get(&reference.provider, &reference.model_id)
        .cloned()
        .ok_or_else(|| {
            AgentError::FailedPrecondition(format!(
                "model {}/{} is not in the catalog",
                reference.provider, reference.model_id
            ))
        })?;
    if !llm.has_credential(&info.provider).await {
        return Err(AgentError::FailedPrecondition(format!(
            "no account is connected for {}; sign in under Settings → Models",
            info.provider
        )));
    }
    Ok(ResolvedModel {
        info,
        thinking: None,
    })
}

fn no_model_available() -> AgentError {
    AgentError::FailedPrecondition(
        "no model is available; connect a provider under Settings → Models".into(),
    )
}

/// Nesting depth: 0 for a top-level conversation, +1 per parent.
pub(crate) async fn depth_of(runtime: &AgentRuntime, record: &ConversationRecord) -> u32 {
    let mut depth = 0;
    let mut parent = record.parent_id.clone();
    while let Some(parent_id) = parent {
        depth += 1;
        if depth > 8 {
            break;
        }
        parent = match runtime.inner.store.conversation(&parent_id).await {
            Ok(Some(row)) => row.parent_id,
            _ => None,
        };
    }
    depth
}

/// The latest user text, the input auto thinking classifies.
pub(crate) fn latest_user_text(entries: &[(u64, Entry)]) -> String {
    entries
        .iter()
        .rev()
        .find_map(|(_, entry)| match entry {
            Entry::User { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(crate) async fn take_turn(
    runtime: &AgentRuntime,
    id: &str,
    memory: &mut RunMemory,
    cancel: &CancellationToken,
) -> Result<TurnOutcome, AgentError> {
    let record = runtime.record(id).await?;
    let chain = conversation_chain(runtime, &record).await?;
    if chain.is_empty() {
        return Err(no_model_available());
    }
    let models: Vec<ModelInfo> = chain
        .iter()
        .map(|candidate| candidate.info.clone())
        .collect();
    compaction::compact_if_needed(runtime, &record, &models, cancel).await?;
    let entries = runtime.inner.store.entries(id).await?;
    let depth = depth_of(runtime, &record).await;
    let find_available = find::available(runtime, id).await;
    let toolset = toolset_for(&record, depth, find_available);
    let files = context_files::context_files(runtime, &record, cancel).await;
    let child_context = entries
        .iter()
        .find_map(|(_, entry)| match entry {
            Entry::SubagentContext { context } => Some(context.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let subagent = record
        .agent
        .as_deref()
        .and_then(SubagentKind::from_name)
        .map(|kind| SubagentPrompt {
            agent_prompt: kind.prompt(),
            context: &child_context,
        });
    let mut system = prompts::system_blocks(&record, &toolset.names(), &files.context, subagent);
    if let Some(goal_entry) = entries
        .iter()
        .rev()
        .find_map(|(_, entry)| match entry {
            Entry::Goal { state } => Some(state),
            _ => None,
        })
        .filter(|state| {
            matches!(
                state.status,
                roost_protocol::wire::agent_chat::GoalStatus::Active
                    | roost_protocol::wire::agent_chat::GoalStatus::BudgetLimited
                    | roost_protocol::wire::agent_chat::GoalStatus::Paused
            )
        })
    {
        let todo_context = entries
            .iter()
            .rev()
            .find_map(|(_, entry)| match entry {
                Entry::Todo { phases } => Some(history::todo_message(phases)),
                _ => None,
            })
            .unwrap_or_default();
        system.push(prompts::render(
            prompts::GOAL_MODE,
            &[
                ("objective", &escape_goal_xml(&goal_entry.objective)),
                ("status", goal_entry.status.as_str()),
                ("todo_context", &escape_goal_xml(&todo_context)),
            ],
        ));
    }
    let mut attempts = Vec::with_capacity(chain.len());
    for candidate in &chain {
        let thinking = resolve_thinking(runtime, &record, candidate, &entries, memory).await;
        let mut messages = history::build_messages(&entries, &candidate.info.provider);
        // Some providers reject assistant prefill, even when its partial output is durable.
        if matches!(messages.last(), Some(Message::Assistant { .. })) {
            messages.push(Message::user_text(prompts::RESUME_INTERRUPTED));
        }
        attempts.push(ChatRequest {
            system: system.clone(),
            messages,
            tools: toolset.specs.clone(),
            thinking: thinking.to_owned(),
            session_id: record.id.clone(),
            max_tokens: None,
            model: candidate.info.clone(),
        });
    }
    let input_through_seq = latest_submission_seq(&entries).unwrap_or(0);
    let captured_goal = entries.iter().rev().find_map(|(_, entry)| match entry {
        Entry::Goal { state } => Some(state),
        _ => None,
    });
    let goal_id = captured_goal.map(|state| state.id.clone());
    let metadata = crate::records::AssistantMetadata {
        completed: false,
        stop_reason: None,
        input_through_seq,
        continued: memory.continued,
        yield_reminders: memory.yield_reminders,
        goal_id: captured_goal
            .filter(|state| {
                matches!(
                    state.status,
                    roost_protocol::wire::agent_chat::GoalStatus::Active
                        | roost_protocol::wire::agent_chat::GoalStatus::BudgetLimited
                )
            })
            .map(|state| state.id.clone()),
    };
    let (_, output, used) =
        model_call::primary_call(runtime, id, &attempts, cancel, metadata).await?;
    runtime.note_processed_seq(id, input_through_seq);
    let calls = output.tool_calls();
    runtime.advisor_turn_ended(id, !calls.is_empty());
    if calls.is_empty() {
        let model = chain.into_iter().nth(used).ok_or_else(no_model_available)?;
        return Ok(TurnOutcome::Text {
            output: Box::new(output),
            model: Box::new(model),
        });
    }
    let end = tool_round::execute_round(
        tool_round::CallScope {
            runtime,
            record: &record,
            toolset: &toolset,
            depth,
            goal_id,
            checkpoint_id: crate::journal::current_checkpoint(&entries),
            cancel,
        },
        calls,
    )
    .await?;
    Ok(TurnOutcome::Tools { end })
}

async fn resolve_thinking(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    model: &ResolvedModel,
    entries: &[(u64, Entry)],
    memory: &mut RunMemory,
) -> &'static str {
    let requested = model
        .thinking
        .clone()
        .unwrap_or_else(|| record.thinking_level.clone());
    if requested != "auto" {
        return model.info.clamp_thinking_level(&requested);
    }
    if let Some(level) = memory.auto_level {
        return model.info.clamp_thinking_level(level);
    }
    let request = latest_user_text(entries);
    let level = auto_thinking::classify(runtime, &record.id, &model.info, &request).await;
    memory.auto_level = Some(level);
    model.info.clamp_thinking_level(level)
}
fn escape_goal_xml(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            character => escaped.push(character),
        }
    }
    escaped
}
