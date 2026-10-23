//! Rewinds user turns and forks independent conversations from journal history.
//! Rewind entries are the single source of truth for transcript and model history.
//! Forks copy effective entries but never copy child-only context or side usage.

use std::time::Duration;

use roost_protocol::wire::agent_chat::{ConversationSummary, TOOL_REWIND_FILES};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::journal::effective_entries;
use crate::projection;
use crate::records::{ConversationRecord, Entry};
use crate::runtime::{AgentRuntime, now_ms, random_id};
use crate::traits::{OwnerConversationId, ToolCall, WorkerConversationNamespace};

impl AgentRuntime {
    /// Hides a user turn and optionally restores its worker-side file changes.
    pub async fn rewind(
        &self,
        id: &str,
        user_item_id: &str,
        restore_files: bool,
    ) -> Result<String, AgentError> {
        let entries = self.inner.store.entries(id).await?;
        let (seq, user_text) = entries
            .iter()
            .find_map(|(sequence, entry)| match entry {
                Entry::User { text, item_id, .. }
                    if item_id.as_deref().map_or_else(
                        || projection::user_item_id(*sequence) == user_item_id,
                        |item_id| item_id == user_item_id,
                    ) =>
                {
                    Some((*sequence, text.clone()))
                }
                _ => None,
            })
            .ok_or_else(|| {
                AgentError::InvalidArgument(format!("user item {user_item_id} not found"))
            })?;
        if self.abort(id).await? {
            while self.is_running(id) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
        let mut restored_files = None;
        if restore_files {
            let record = self.record(id).await?;
            let (output, _discarded) = mpsc::channel(1);
            let result = self
                .inner
                .tools
                .execute(
                    &record.worker_fp,
                    ToolCall {
                        call_id: random_id("rewind-"),
                        conversation_id: WorkerConversationNamespace(record.id.clone()),
                        owner_conversation_id: OwnerConversationId(record.id),
                        cwd: record.cwd,
                        tool: TOOL_REWIND_FILES.to_owned(),
                        args_json: serde_json::json!({"checkpoint_id": seq}).to_string(),
                        timeout_ms: 120_000,
                        checkpoint_id: seq,
                    },
                    output,
                    CancellationToken::new(),
                )
                .await
                .map_err(AgentError::FailedPrecondition)?;
            if result.is_error {
                return Err(AgentError::FailedPrecondition(result.content));
            }
            restored_files = Some(result.content);
        }
        let has_later_user = effective_entries(&entries)
            .iter()
            .any(|(entry_seq, entry)| *entry_seq >= seq && matches!(entry, Entry::User { .. }));
        if has_later_user {
            self.append(id, Entry::Rewind { to_seq: seq }).await?;
        }
        if let Some(files) = restored_files {
            self.notice(id, "info", "Files restored", &files).await;
        }
        tracing::info!(
            conversation_id = id,
            checkpoint_id = seq,
            restore_files,
            "agent conversation rewound"
        );
        Ok(user_text)
    }

    /// Creates a top-level conversation with a copy of effective journal history.
    pub async fn fork(
        &self,
        id: &str,
        item_id: Option<&str>,
    ) -> Result<ConversationSummary, AgentError> {
        let source = self.record(id).await?;
        let entries = self.inner.store.entries(id).await?;
        let effective = effective_entries(&entries);
        let empty_tool_index = projection::ToolCallIndex::new();
        let through_seq = if let Some(item_id) = item_id {
            effective
                .iter()
                .rev()
                .find_map(|(seq, entry)| {
                    projection::entry_item(*seq, entry, &empty_tool_index)
                        .filter(|item| projection::item_id(item) == item_id)
                        .map(|_| *seq)
                })
                .ok_or_else(|| {
                    AgentError::InvalidArgument(format!("transcript item {item_id} not found"))
                })?
        } else {
            u64::MAX
        };
        let now = now_ms();
        let mut child = ConversationRecord {
            id: random_id("c-"),
            title: format!("{} (fork)", source.title),
            worker_fp: source.worker_fp,
            worker_label: source.worker_label,
            worker_os: source.worker_os,
            cwd: source.cwd,
            model: source.model,
            thinking_level: source.thinking_level,
            mode: source.mode,
            read_only: source.read_only,
            pre_plan_model: source.pre_plan_model,
            parent_id: None,
            agent: None,
            advisor: None,
            run_state: roost_protocol::wire::agent_chat::AgentRunState::Idle,
            error: None,
            created_ms: now,
            updated_ms: now,
            revision: 0,
        };
        child.revision = self.inner.store.save_conversation(&child, 0).await?;
        let mut sequence_mapping = Vec::with_capacity(effective.len());
        for (source_seq, mut entry) in effective.into_iter().filter(|(seq, entry)| {
            *seq <= through_seq
                && !matches!(
                    entry,
                    Entry::SubagentContext { .. } | Entry::ExtraUsage { .. }
                )
        }) {
            remap_sequence_fields(&mut entry, &sequence_mapping);
            match self
                .inner
                .store
                .append_entry(&child.id, &entry, &[], None)
                .await
            {
                Ok(destination_seq) => sequence_mapping.push((source_seq, destination_seq)),
                Err(error) => {
                    let _ = self.inner.store.delete_conversation(&child.id).await;
                    return Err(error);
                }
            }
        }

        tracing::info!(conversation_id = %child.id, source_conversation_id = id, item_id = ?item_id, "agent conversation forked");
        self.publish_summary(&child).await;
        Ok(self.summary(&child).await)
    }
}

fn remap_sequence_fields(entry: &mut Entry, mapping: &[(u64, u64)]) {
    match entry {
        Entry::InputConsumed { through_seq } => {
            *through_seq = map_sequence_floor(*through_seq, mapping);
        }
        Entry::Assistant {
            metadata: Some(metadata),
            ..
        } => {
            metadata.input_through_seq = map_sequence_floor(metadata.input_through_seq, mapping);
        }
        Entry::RunIntent { through_seq, .. } => {
            *through_seq = map_sequence_floor(*through_seq, mapping);
        }
        Entry::GoalContinuation {
            input_through_seq, ..
        } => {
            *input_through_seq = map_sequence_floor(*input_through_seq, mapping);
        }
        Entry::Compaction { first_kept_seq, .. } => {
            *first_kept_seq = map_sequence_ceiling(*first_kept_seq, mapping);
        }
        Entry::Rewind { to_seq } => {
            *to_seq = map_sequence_ceiling(*to_seq, mapping);
        }
        _ => {}
    }
}

fn map_sequence_floor(source_seq: u64, mapping: &[(u64, u64)]) -> u64 {
    let mapped = mapping.partition_point(|(original, _)| *original <= source_seq);
    mapped.checked_sub(1).map_or(0, |idx| mapping[idx].1)
}

fn map_sequence_ceiling(source_seq: u64, mapping: &[(u64, u64)]) -> u64 {
    let mapped = mapping.partition_point(|(original, _)| *original < source_seq);
    mapping.get(mapped).map_or_else(
        || {
            mapping
                .last()
                .map_or(1, |(_, destination)| destination.saturating_add(1))
        },
        |(_, destination)| *destination,
    )
}
#[cfg(test)]
mod tests {
    use super::{map_sequence_ceiling, remap_sequence_fields};
    use crate::records::Entry;

    #[test]
    fn fork_compaction_boundary_uses_first_copied_source_at_or_after_it() {
        let mapping = [(2, 1), (7, 2), (12, 3)];
        let mut compaction = Entry::Compaction {
            summary: "summary".to_owned(),
            first_kept_seq: 4,
        };

        remap_sequence_fields(&mut compaction, &mapping);

        assert!(matches!(
            compaction,
            Entry::Compaction {
                first_kept_seq: 2,
                ..
            }
        ));
        assert_eq!(map_sequence_ceiling(13, &mapping), 4);
    }
}
