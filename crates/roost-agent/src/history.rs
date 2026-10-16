//! Entry journal → provider messages for the next model call. Applies the
//! latest compaction, delivers advisories as user-role notes, drops notices,
//! and repairs tool calls a cancelled run left without results.

use std::collections::{HashMap, HashSet};

use roost_llm::{AssistantBlock, Message};

use crate::job_notice::{background_job_message, summary_text};
use crate::records::{AdvisorySeverity, Entry};
use crate::todo_notice::todo_message;

/// The text an aborted or lost tool call answers with, so the provider sees
/// every `tool_use` paired with a result.
const MISSING_RESULT: &str = "Tool call did not complete: the run was interrupted.";

/// The model-facing form of a delivered advisory.
pub fn advisory_message(severity: AdvisorySeverity, note: &str) -> String {
    format!(
        "<advisory severity=\"{}\" guidance=\"weigh, don't blindly obey\">{note}</advisory>",
        severity.as_str()
    )
}

/// The entries after the latest compaction, and that compaction's summary.
pub fn active_window(entries: &[(u64, Entry)]) -> (Option<&str>, &[(u64, Entry)]) {
    let latest = entries
        .iter()
        .enumerate()
        .rev()
        .find_map(|(position, (_, entry))| match entry {
            Entry::Compaction {
                summary,
                first_kept_seq,
            } => Some((position, summary.as_str(), *first_kept_seq)),
            _ => None,
        });
    let Some((_, summary, first_kept_seq)) = latest else {
        return (None, entries);
    };
    let start = entries
        .iter()
        .position(|(seq, _)| *seq >= first_kept_seq)
        .unwrap_or(entries.len());
    (Some(summary), &entries[start..])
}

/// Provider messages for a call to `provider`. Thinking blocks from another
/// provider are dropped: their signatures are meaningless elsewhere.
pub fn build_messages(entries: &[(u64, Entry)], provider: &str) -> Vec<Message> {
    let entries = &crate::journal::delivery_order(crate::journal::effective_entries(entries));
    let (summary, window) = active_window(entries);
    let latest_copies = latest_item_copies(window);
    let latest_todo = window
        .iter()
        .rposition(|(_, entry)| matches!(entry, Entry::Todo { .. }));
    let active_goal_id = window
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
            )
        })
        .map(|state| state.id.as_str());
    let pending_goal_continuation =
        window
            .iter()
            .enumerate()
            .rev()
            .find_map(|(position, (_, entry))| {
                let Entry::GoalContinuation { goal_id, .. } = entry else {
                    return None;
                };
                (active_goal_id == Some(goal_id.as_str())
                    && !window[position + 1..]
                        .iter()
                        .any(|(_, later)| matches!(later, Entry::Assistant { .. })))
                .then_some((position, goal_id.as_str()))
            });
    let mut messages = Vec::new();
    if let Some(summary) = summary {
        messages.push(Message::user_text(format!(
            "<conversation-summary>\nThe earlier conversation was compacted. Summary:\n{summary}\n</conversation-summary>"
        )));
    }
    let mut open_calls: Vec<(String, String)> = Vec::new();
    let mut deferred_context = Vec::new();
    for (position, (_, entry)) in window.iter().enumerate() {
        match entry {
            Entry::GoalContinuation {
                goal_id,
                budget_wrap_up,
                ..
            } if pending_goal_continuation == Some((position, goal_id.as_str())) => {
                append_context_message(
                    &mut messages,
                    &mut deferred_context,
                    &open_calls,
                    Message::user_text(if *budget_wrap_up {
                        "<goal-budget-wrap-up>Provide a concise progress report, evidence gathered, blockers, and next step. Do not start substantive work.</goal-budget-wrap-up>"
                    } else {
                        "<goal-continuation>Continue the active goal using its current objective and state. This is hidden control context, not a new user message.</goal-continuation>"
                    }),
                );
            }
            Entry::User { text, .. } => append_context_message(
                &mut messages,
                &mut deferred_context,
                &open_calls,
                Message::user_text(text.clone()),
            ),
            Entry::Assistant { blocks, model, .. } => {
                if !open_calls.is_empty() {
                    close_open_calls(&mut messages, &mut open_calls);
                    messages.append(&mut deferred_context);
                }
                let same_provider = model
                    .as_ref()
                    .is_some_and(|reference| reference.provider == provider);
                let kept: Vec<AssistantBlock> = blocks
                    .iter()
                    .filter(|block| {
                        same_provider || !matches!(block, AssistantBlock::Thinking { .. })
                    })
                    .filter(|block| match block {
                        AssistantBlock::Text { text } => !text.is_empty(),
                        _ => true,
                    })
                    .cloned()
                    .collect();
                if kept.is_empty() {
                    continue;
                }
                for block in &kept {
                    if let AssistantBlock::ToolCall { call_id, name, .. } = block {
                        open_calls.push((call_id.clone(), name.clone()));
                    }
                }
                messages.push(Message::Assistant { blocks: kept });
            }
            Entry::ToolResult {
                call_id,
                tool_name,
                text,
                is_error,
                ..
            } => {
                let Some(open) = open_calls
                    .iter()
                    .position(|(open_id, _)| open_id == call_id)
                else {
                    continue;
                };
                open_calls.remove(open);
                messages.push(Message::ToolResult {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    text: text.clone(),
                    is_error: *is_error,
                });
                if open_calls.is_empty() {
                    messages.append(&mut deferred_context);
                }
            }
            Entry::Advisory {
                severity,
                note,
                delivered: true,
                ..
            } if latest_copies.contains(&position) => append_context_message(
                &mut messages,
                &mut deferred_context,
                &open_calls,
                Message::user_text(advisory_message(*severity, note)),
            ),
            Entry::BackgroundJob {
                job_id,
                command,
                state,
                output,
                exit_code,
                ..
            } => append_context_message(
                &mut messages,
                &mut deferred_context,
                &open_calls,
                Message::user_text(background_job_message(
                    job_id, command, *state, output, *exit_code,
                )),
            ),
            Entry::Todo { phases } if latest_todo == Some(position) => {
                append_context_message(
                    &mut messages,
                    &mut deferred_context,
                    &open_calls,
                    Message::user_text(todo_message(phases)),
                );
            }
            Entry::Ask {
                answers: Some(answers),
                ..
            } if latest_copies.contains(&position) => append_context_message(
                &mut messages,
                &mut deferred_context,
                &open_calls,
                Message::user_text(format!(
                    "<answers>{}</answers>",
                    serde_json::to_string(answers).unwrap_or_default()
                )),
            ),
            Entry::Ask { .. }
            | Entry::Btw { .. }
            | Entry::Rewind { .. }
            | Entry::ExtraUsage { .. }
            | Entry::RunIntent { .. }
            | Entry::LoopState { .. }
            | Entry::Goal { .. }
            | Entry::GoalContinuation { .. }
            | Entry::GoalActivity { .. }
            | Entry::Advisory { .. }
            | Entry::Todo { .. }
            | Entry::SubagentContext { .. }
            | Entry::Compaction { .. }
            | Entry::ModeChange { .. }
            | Entry::PlanProposal { .. }
            | Entry::Notice { .. } => {}
        }
    }
    close_open_calls(&mut messages, &mut open_calls);
    messages.append(&mut deferred_context);
    messages
}

fn append_context_message(
    messages: &mut Vec<Message>,
    deferred_context: &mut Vec<Message>,
    open_calls: &[(String, String)],
    message: Message,
) {
    if open_calls.is_empty() {
        messages.push(message);
    } else {
        deferred_context.push(message);
    }
}

fn close_open_calls(messages: &mut Vec<Message>, open_calls: &mut Vec<(String, String)>) {
    for (call_id, tool_name) in open_calls.drain(..) {
        messages.push(Message::ToolResult {
            call_id,
            tool_name,
            text: MISSING_RESULT.into(),
            is_error: true,
        });
    }
}

/// Positions of the latest copy of each advisory and ask item.
fn latest_item_copies(window: &[(u64, Entry)]) -> HashSet<usize> {
    let mut latest: HashMap<&str, usize> = HashMap::new();
    for (position, (_, entry)) in window.iter().enumerate() {
        match entry {
            Entry::Advisory { item_id, .. } | Entry::Ask { item_id, .. } => {
                latest.insert(item_id.as_str(), position);
            }
            _ => {}
        }
    }
    latest.into_values().collect()
}

/// A rough token estimate (4 bytes per token) for sizing compaction.
pub fn estimate_tokens(entry: &Entry) -> u64 {
    let bytes = match entry {
        Entry::User { text, .. } => text.len(),
        Entry::Assistant { blocks, .. } => blocks
            .iter()
            .map(|block| match block {
                AssistantBlock::Text { text } | AssistantBlock::Thinking { text, .. } => text.len(),
                AssistantBlock::ToolCall {
                    args_json, name, ..
                } => args_json.len() + name.len(),
            })
            .sum(),
        Entry::ToolResult { text, .. } => text.len(),
        Entry::Advisory { note, .. } => note.len(),
        Entry::BackgroundJob {
            command, output, ..
        } => command.len() + output.len(),
        Entry::Compaction { summary, .. } => summary.len(),
        Entry::Todo { phases } => todo_message(phases).len(),
        Entry::SubagentContext { context } => context.len(),
        Entry::Ask { questions, .. } => questions
            .iter()
            .map(|question| question.question.len())
            .sum(),
        Entry::ModeChange { .. }
        | Entry::PlanProposal { .. }
        | Entry::Notice { .. }
        | Entry::Btw { .. }
        | Entry::Rewind { .. }
        | Entry::ExtraUsage { .. }
        | Entry::RunIntent { .. }
        | Entry::LoopState { .. }
        | Entry::Goal { .. }
        | Entry::GoalContinuation { .. }
        | Entry::GoalActivity { .. } => 0,
    };
    (bytes as u64).div_ceil(4)
}

/// Renders entries as plain text for the compaction summariser.
pub fn render_for_summary(entries: &[(u64, Entry)]) -> String {
    let mut rendered = String::new();
    for (_, entry) in entries {
        match entry {
            Entry::User { text, .. } => rendered.push_str(&format!("\n[user]\n{text}\n")),
            Entry::Assistant { blocks, .. } => {
                for block in blocks {
                    match block {
                        AssistantBlock::Text { text } => {
                            rendered.push_str(&format!("\n[assistant]\n{text}\n"));
                        }
                        AssistantBlock::ToolCall {
                            name, args_json, ..
                        } => {
                            rendered.push_str(&format!("\n[tool call] {name}({args_json})\n"));
                        }
                        AssistantBlock::Thinking { .. } => {}
                    }
                }
            }
            Entry::ToolResult {
                tool_name, text, ..
            } => {
                let shown: String = text.chars().take(4096).collect();
                rendered.push_str(&format!("\n[{tool_name} result]\n{shown}\n"));
            }
            Entry::Compaction { summary, .. } => {
                rendered.push_str(&format!("\n[earlier summary]\n{summary}\n"));
            }
            Entry::Advisory { note, .. } => rendered.push_str(&format!("\n[advisory] {note}\n")),
            Entry::BackgroundJob {
                job_id,
                command,
                output,
                ..
            } => rendered.push_str(&summary_text(job_id, command, output)),
            Entry::Todo { phases } => rendered.push_str(&format!("\n{}\n", todo_message(phases))),
            Entry::Ask {
                questions, answers, ..
            } => {
                for question in questions {
                    rendered.push_str(&format!("\n[asked the user] {}\n", question.question));
                }
                if let Some(answers) = answers {
                    rendered.push_str(&format!(
                        "[user answered] {}\n",
                        serde_json::to_string(answers).unwrap_or_default()
                    ));
                }
            }
            Entry::ModeChange { .. }
            | Entry::PlanProposal { .. }
            | Entry::Notice { .. }
            | Entry::Btw { .. }
            | Entry::ExtraUsage { .. }
            | Entry::SubagentContext { .. }
            | Entry::Rewind { .. }
            | Entry::RunIntent { .. }
            | Entry::LoopState { .. }
            | Entry::Goal { .. }
            | Entry::GoalContinuation { .. }
            | Entry::GoalActivity { .. } => {}
        }
    }
    rendered
}
