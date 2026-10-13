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
use crate::traits::ToolCall;

impl AgentRuntime {
    /// Hides a user turn and optionally restores its worker-side file changes.
    pub async fn rewind(
        &self,
        id: &str,
        user_item_id: &str,
        restore_files: bool,
    ) -> Result<String, AgentError> {
        let seq = user_item_id
            .strip_prefix('u')
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| AgentError::InvalidArgument("rewind needs a user item id".into()))?;
        let entries = self.inner.store.entries(id).await?;
        let user_text = entries
            .iter()
            .find_map(|(entry_seq, entry)| {
                (*entry_seq == seq)
                    .then_some(entry)
                    .and_then(|entry| match entry {
                        Entry::User { text, .. } => Some(text.clone()),
                        _ => None,
                    })
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
                        conversation_id: record.id,
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
        for (_, entry) in effective.into_iter().filter(|(seq, _)| *seq <= through_seq) {
            if matches!(
                entry,
                Entry::SubagentContext { .. } | Entry::ExtraUsage { .. }
            ) {
                continue;
            }
            if let Err(error) = self
                .inner
                .store
                .append_entry(&child.id, &entry, &[], None)
                .await
            {
                let _ = self.inner.store.delete_conversation(&child.id).await;
                return Err(error);
            }
        }
        tracing::info!(conversation_id = %child.id, source_conversation_id = id, item_id = ?item_id, "agent conversation forked");
        self.publish_summary(&child).await;
        Ok(self.summary(&child).await)
    }
}
