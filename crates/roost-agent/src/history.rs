//! Entry journal → provider messages for the next model call. Applies the
//! latest compaction, delivers advisories as user-role notes, drops notices,
//! and repairs tool calls a cancelled run left without results.

use std::collections::{HashMap, HashSet};

use roost_llm::{AssistantBlock, Message};

use crate::records::{AdvisorySeverity, Entry};

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
    let (summary, window) = active_window(entries);
    let latest_copies = latest_item_copies(window);
    let mut messages = Vec::new();
    if let Some(summary) = summary {
        messages.push(Message::user_text(format!(
            "<conversation-summary>\nThe earlier conversation was compacted. Summary:\n{summary}\n</conversation-summary>"
        )));
    }
    let mut open_calls: Vec<(String, String)> = Vec::new();
    for (position, (_, entry)) in window.iter().enumerate() {
        if !matches!(entry, Entry::ToolResult { .. }) {
            close_open_calls(&mut messages, &mut open_calls);
        }
        match entry {
            Entry::User { text } => messages.push(Message::user_text(text.clone())),
            Entry::Assistant { blocks, model, .. } => {
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
            }
            Entry::Advisory {
                severity,
                note,
                delivered: true,
                ..
            } if latest_copies.contains(&position) => {
                messages.push(Message::user_text(advisory_message(*severity, note)));
            }
            Entry::Advisory { .. }
            | Entry::Compaction { .. }
            | Entry::ModeChange { .. }
            | Entry::PlanProposal { .. }
            | Entry::Notice { .. } => {}
        }
    }
    close_open_calls(&mut messages, &mut open_calls);
    messages
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

/// Positions of the latest copy of each advisory item.
fn latest_item_copies(window: &[(u64, Entry)]) -> HashSet<usize> {
    let mut latest: HashMap<&str, usize> = HashMap::new();
    for (position, (_, entry)) in window.iter().enumerate() {
        if let Entry::Advisory { item_id, .. } = entry {
            latest.insert(item_id.as_str(), position);
        }
    }
    latest.into_values().collect()
}

/// A rough token estimate (4 bytes per token) for sizing compaction.
pub fn estimate_tokens(entry: &Entry) -> u64 {
    let bytes = match entry {
        Entry::User { text } => text.len(),
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
        Entry::Compaction { summary, .. } => summary.len(),
        Entry::ModeChange { .. } | Entry::PlanProposal { .. } | Entry::Notice { .. } => 0,
    };
    (bytes as u64).div_ceil(4)
}

/// Renders entries as plain text for the compaction summariser.
pub fn render_for_summary(entries: &[(u64, Entry)]) -> String {
    let mut rendered = String::new();
    for (_, entry) in entries {
        match entry {
            Entry::User { text } => rendered.push_str(&format!("\n[user]\n{text}\n")),
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
            Entry::ModeChange { .. } | Entry::PlanProposal { .. } | Entry::Notice { .. } => {}
        }
    }
    rendered
}
