//! Roost's existing system and goal prompts as ordered durable sections.
//!
//! Durable renders these for every model request; this adapter rereads the
//! current conversation journal and worker context instead of caching a turn.

use std::sync::Arc;

use rpi_durable::chord::context::Context;
use rpi_durable::harness::types::{PromptRenderFuture, ToolCaller};
use rpi_durable::harness::{PromptInput, PromptRender, PromptSection};
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::records::{ConversationRecord, Entry};
use crate::runtime::AgentRuntime;
use crate::toolset::SubagentKind;

const SYSTEM_SECTION: &str = "roost-system";
const GOAL_SECTION: &str = "roost-goal";

/// Build the original Roost prompt in stable system-before-goal order.
pub(crate) fn sections(runtime: AgentRuntime, conversation_id: String) -> Vec<PromptSection> {
    let system_runtime = runtime.clone();
    let system_conversation = conversation_id.clone();
    let system: PromptRender = Arc::new(move |input, context| {
        let runtime = system_runtime.clone();
        let conversation_id = system_conversation.clone();
        Box::pin(async move {
            match render_system_blocks(&runtime, &conversation_id, &input, &context).await {
                Ok(prompt) => prompt,
                Err(error) => {
                    tracing::warn!(conversation_id, %error, "could not render Roost system prompt");
                    None
                }
            }
        }) as PromptRenderFuture
    });
    let goal_runtime = runtime;
    let goal: PromptRender = Arc::new(move |_input, _context| {
        let runtime = goal_runtime.clone();
        let conversation_id = conversation_id.clone();
        Box::pin(async move {
            match render_goal(&runtime, &conversation_id).await {
                Ok(prompt) => prompt,
                Err(error) => {
                    tracing::warn!(conversation_id, %error, "could not render Roost goal prompt");
                    None
                }
            }
        }) as PromptRenderFuture
    });
    vec![
        PromptSection {
            key: SYSTEM_SECTION.into(),
            render: system,
            tag: false,
        },
        PromptSection {
            key: GOAL_SECTION.into(),
            render: goal,
            tag: false,
        },
    ]
}

async fn render_system_blocks(
    runtime: &AgentRuntime,
    conversation_id: &str,
    input: &PromptInput,
    context: &Context,
) -> Result<Option<String>, AgentError> {
    let (record, entries) = fresh_context(runtime, conversation_id).await?;
    let cancellation = cancellation_token(context);
    let files = crate::context_files::context_files(runtime, &record, &cancellation).await;
    let child_context = entries
        .iter()
        .find_map(|(_, entry)| match entry {
            Entry::SubagentContext { context } => Some(context.as_str()),
            _ => None,
        })
        .unwrap_or_default();
    let subagent = record
        .agent
        .as_deref()
        .and_then(SubagentKind::from_name)
        .map(|kind| crate::prompts::SubagentPrompt {
            agent_prompt: kind.prompt(),
            context: child_context,
        });
    let tool_names: Vec<String> = input
        .agent
        .tools
        .iter()
        .filter(|tool| tool.available_to(ToolCaller::Model))
        .map(|tool| tool.name().to_owned())
        .collect();
    let rendered = crate::prompts::system_blocks(&record, &tool_names, &files.context, subagent);
    let prompt = rendered.join("\n\n");
    Ok((!prompt.is_empty()).then_some(prompt))
}
async fn render_goal(
    runtime: &AgentRuntime,
    conversation_id: &str,
) -> Result<Option<String>, AgentError> {
    let (_, entries) = fresh_context(runtime, conversation_id).await?;
    let Some(goal) = entries.iter().rev().find_map(|(_, entry)| match entry {
        Entry::Goal { state } => Some(state),
        _ => None,
    }) else {
        return Ok(None);
    };
    if !matches!(
        goal.status,
        roost_protocol::wire::agent_chat::GoalStatus::Active
            | roost_protocol::wire::agent_chat::GoalStatus::BudgetLimited
            | roost_protocol::wire::agent_chat::GoalStatus::Paused
    ) {
        return Ok(None);
    }
    let todo_context = entries
        .iter()
        .rev()
        .find_map(|(_, entry)| match entry {
            Entry::Todo { phases } => Some(crate::todo_notice::todo_message(phases)),
            _ => None,
        })
        .unwrap_or_default();
    Ok(Some(crate::prompts::render(
        crate::prompts::GOAL_MODE,
        &[
            ("objective", &crate::turn::escape_goal_xml(&goal.objective)),
            ("status", goal.status.as_str()),
            ("todo_context", &crate::turn::escape_goal_xml(&todo_context)),
        ],
    )))
}

async fn fresh_context(
    runtime: &AgentRuntime,
    conversation_id: &str,
) -> Result<(ConversationRecord, Vec<(u64, Entry)>), AgentError> {
    let record = runtime.record(conversation_id).await?;
    let entries = runtime.inner.store.entries(conversation_id).await?;
    Ok((record, crate::journal::effective_entries(&entries)))
}

fn cancellation_token(context: &Context) -> CancellationToken {
    context
        .abort_signal()
        .map(|signal| signal.token())
        .unwrap_or_default()
}
