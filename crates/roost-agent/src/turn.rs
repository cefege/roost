//! One turn of a conversation: compaction check, model and thinking-level
//! resolution, request assembly, the streamed model call, then the tool round
//! its tool calls ask for. The run loop repeats turns until one ends the run.

use roost_llm::ChatRequest;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::model_call::{self, ModelOutput};
use crate::prompts::{self, SubagentPrompt};
use crate::records::{ConversationRecord, Entry, Role};
use crate::roles::{self, ResolvedModel};
use crate::runtime::{AgentRuntime, RunEnd};
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

/// The model a conversation calls: its own, else the `default` role.
pub(crate) async fn conversation_model(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
) -> Result<ResolvedModel, AgentError> {
    let llm = runtime.inner.llm.as_ref();
    if let Some(reference) = &record.model {
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
        return Ok(ResolvedModel {
            info,
            thinking: None,
        });
    }
    let settings = runtime.inner.store.settings().await?;
    roles::resolve_role(llm, &settings, Role::Default)
        .await
        .ok_or_else(|| {
            AgentError::FailedPrecondition(
                "no model is available; connect a provider under Settings → Models".into(),
            )
        })
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
            Entry::User { text } => Some(text.clone()),
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
    let model = conversation_model(runtime, &record).await?;
    compaction::compact_if_needed(runtime, &record, &model.info, cancel).await?;
    let entries = runtime.inner.store.entries(id).await?;
    let thinking = resolve_thinking(runtime, &record, &model, &entries, memory).await;
    let depth = depth_of(runtime, &record).await;
    let find_available = find::available(runtime, &model.info).await;
    let toolset = toolset_for(&record, depth, find_available);
    let files = context_files::context_files(runtime, &record, cancel).await;
    let child_context = runtime.child_context(id).unwrap_or_default();
    let subagent = record
        .agent
        .as_deref()
        .and_then(SubagentKind::from_name)
        .map(|kind| SubagentPrompt {
            agent_prompt: kind.prompt(),
            context: &child_context,
        });
    let request = ChatRequest {
        system: prompts::system_blocks(&record, &toolset.names(), &files.context, subagent),
        messages: history::build_messages(&entries, &model.info.provider),
        tools: toolset.specs.clone(),
        thinking: thinking.to_owned(),
        session_id: record.id.clone(),
        max_tokens: None,
        model: model.info.clone(),
    };
    let (_, output) = model_call::primary_call(runtime, id, &request, cancel).await?;
    let calls = output.tool_calls();
    runtime.advisor_turn_ended(id, !calls.is_empty());
    if calls.is_empty() {
        return Ok(TurnOutcome::Text {
            output: Box::new(output),
            model: Box::new(model),
        });
    }
    let end = tool_round::execute_round(runtime, &record, &toolset, depth, calls, cancel).await?;
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
