//! Plain-text views of journal entries for compaction: the token estimate that
//! sizes the window a compaction keeps, and the transcript its summariser reads.
//! Called by `compaction.rs` and `btw.rs`; the wording of delivered notices comes
//! from the notice modules, so the summariser sees what the model saw.

use roost_llm::AssistantBlock;

use crate::job_notice::summary_text;
use crate::peer_notice::peer_summary_text;
use crate::records::Entry;
use crate::todo_notice::todo_message;

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
        Entry::PeerMessage { from, text, .. } => from.len() + text.len(),
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
        | Entry::QueuedInput { .. }
        | Entry::QueuedInputWithdrawn { .. }
        | Entry::InputConsumed { .. }
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
            Entry::PeerMessage { from, text, .. } => {
                rendered.push_str(&peer_summary_text(from, text))
            }
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
            | Entry::QueuedInput { .. }
            | Entry::QueuedInputWithdrawn { .. }
            | Entry::InputConsumed { .. }
            | Entry::RunIntent { .. }
            | Entry::LoopState { .. }
            | Entry::Goal { .. }
            | Entry::GoalContinuation { .. }
            | Entry::GoalActivity { .. } => {}
        }
    }
    rendered
}
