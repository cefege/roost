//! Durable transcript entries projected into Roost's existing chat journal.
//! JournalStorage settles these rows with the kernel batch, then publishes them.
//! Imported host entries are never projected back, preventing a second history.

use rpi_ai::types::{Message, StopReason, UserContent};
use rpi_durable::{EntryId, EntryRecord, ROOT_CONVERSATION_ID, StorageWrite};

use super::state::ProjectionState;
use crate::error::AgentError;
use crate::records::Entry;

pub(super) fn projected_ids(writes: &[StorageWrite]) -> Vec<EntryId> {
    writes
        .iter()
        .filter_map(|write| match write {
            StorageWrite::Entry { value } if projects(value) => Some(value.id),
            _ => None,
        })
        .collect()
}

pub(super) fn entries(
    writes: &[StorageWrite],
    state: &ProjectionState,
) -> Result<Vec<Entry>, AgentError> {
    let mut entries = writes
        .iter()
        .filter_map(|write| match write {
            StorageWrite::Entry { value } if projects(value) => Some(project(value, state)),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.extend(super::nested_projection::nested_entries(writes)?);
    let mut usage = state
        .extra_usage
        .lock()
        .map_err(|_| AgentError::Store("Durable usage projection lock poisoned".into()))?;
    for write in writes {
        if let StorageWrite::Task { value } = write
            && let Some(mut attempts) = usage.remove(&value.id.get())
        {
            entries.append(&mut attempts);
        }
    }
    Ok(entries)
}

fn projects(entry: &EntryRecord) -> bool {
    entry.conversation_id == ROOT_CONVERSATION_ID
        && matches!(
            entry.kind.as_str(),
            rpi_durable::entries::ASSISTANT_ENTRY
                | rpi_durable::entries::TOOL_RESULT_ENTRY
                | rpi_durable::entries::USER_ENTRY
                | rpi_durable::entries::COMPACTION_ENTRY
        )
}

pub(super) fn project(entry: &EntryRecord, state: &ProjectionState) -> Result<Entry, AgentError> {
    let messages = entry.model.as_deref().unwrap_or_default();
    match entry.kind.as_str() {
        rpi_durable::entries::ASSISTANT_ENTRY => {
            let message = messages
                .iter()
                .find_map(|message| match message {
                    Message::Assistant(message) => Some(message),
                    _ => None,
                })
                .ok_or_else(|| malformed(entry, "assistant message"))?;
            let mut metadata = state
                .metadata
                .lock()
                .map_err(|_| malformed(entry, "request metadata lock"))?
                .clone();
            metadata.completed = matches!(
                message.stop_reason,
                StopReason::Stop | StopReason::Length | StopReason::ToolUse
            );
            metadata.stop_reason = Some(native_stop(message));
            let item_id = state
                .item_id
                .lock()
                .map_err(|_| malformed(entry, "stream identity lock"))?
                .take()
                .unwrap_or_else(|| format!("durable-entry-{}", entry.id));
            Ok(Entry::Assistant {
                item_id,
                blocks: super::message::from_durable_blocks(&message.content)?,
                usage: roost_llm::Usage {
                    input: message.usage.input,
                    output: message.usage.output,
                    cache_read: message.usage.cache_read,
                    cache_write: message.usage.cache_write,
                },
                model: Some(crate::records::model_ref(&message.provider, &message.model)),
                metadata: Some(metadata),
            })
        }
        rpi_durable::entries::TOOL_RESULT_ENTRY => {
            let (source, message) = messages
                .iter()
                .find_map(|source| match source {
                    Message::ToolResult(message) => Some((source, message)),
                    _ => None,
                })
                .ok_or_else(|| malformed(entry, "tool-result message"))?;
            let roost_llm::Message::ToolResult {
                call_id,
                tool_name,
                text,
                is_error,
                content,
                provider_data,
            } = super::message::from_durable_message(source, true)?
            else {
                return Err(malformed(entry, "converted tool-result message"));
            };
            let cancelled = entry
                .data
                .as_ref()
                .and_then(|data| data.get("diagnostics"))
                .and_then(serde_json::Value::as_array)
                .is_some_and(|diagnostics| {
                    diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic["code"] == "aborted")
                });
            let details_json = if cancelled {
                serde_json::to_string(&serde_json::json!({
                    "cancelled": true,
                    "details": message.details,
                    "diagnostics": entry.data.as_ref().and_then(|data| data.get("diagnostics")),
                }))
            } else {
                serde_json::to_string(
                    message
                        .details
                        .as_ref()
                        .unwrap_or(&serde_json::Value::Object(Default::default())),
                )
            }
            .map_err(|error| AgentError::Store(error.to_string()))?;
            Ok(Entry::ToolResult {
                call_id,
                tool_name,
                text,
                is_error: is_error && !cancelled,
                details_json,
                content,
                provider_data,
            })
        }
        rpi_durable::entries::USER_ENTRY => Ok(Entry::User {
            text: user_text(messages),
            attachments: Vec::new(),
            loop_repeat: false,
            item_id: Some(format!("durable-entry-{}", entry.id)),
        }),
        rpi_durable::entries::COMPACTION_ENTRY => {
            let head = entry
                .head
                .ok_or_else(|| malformed(entry, "compaction head"))?;
            let origins = state
                .origins
                .lock()
                .map_err(|_| malformed(entry, "origin lock"))?;
            let first_kept_seq = origins
                .range(head..)
                .next()
                .map(|(_, sequence)| *sequence)
                .unwrap_or_else(|| {
                    origins
                        .values()
                        .copied()
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1)
                });
            let text = user_text(messages);
            let summary = text.strip_prefix(
                "The conversation history before this point was compacted into the following summary:\n\n<summary>\n"
            ).and_then(|text| text.strip_suffix("\n</summary>"))
                .unwrap_or(&text).to_owned();
            Ok(Entry::Compaction {
                summary,
                first_kept_seq,
            })
        }
        _ => Err(malformed(entry, "known projected kind")),
    }
}

fn user_text(messages: &[Message]) -> String {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(message) => Some(match &message.content {
                UserContent::Text(text) => text.clone(),
                UserContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        rpi_ai::types::UserContentBlock::Text(text) => Some(text.text.as_str()),
                        rpi_ai::types::UserContentBlock::Image(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            }),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn native_stop(message: &rpi_ai::types::AssistantMessage) -> roost_llm::StopReason {
    match message.stop_reason {
        StopReason::Stop => roost_llm::StopReason::EndTurn,
        StopReason::Length => roost_llm::StopReason::MaxTokens,
        StopReason::ToolUse => roost_llm::StopReason::ToolUse,
        _ => roost_llm::StopReason::Error(
            message
                .error_message
                .clone()
                .unwrap_or_else(|| format!("Model response ended with {:?}", message.stop_reason)),
        ),
    }
}

fn malformed(entry: &EntryRecord, expected: &str) -> AgentError {
    AgentError::Store(format!("Durable entry {} lacks {expected}", entry.id))
}
