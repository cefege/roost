//! What the runtime tells clients: conversation summaries, transcript items
//! for committed entries, notices and usage totals. Every journal write that
//! clients see goes through `append`/`append_guarded` here.

use roost_protocol::wire::agent_chat::{ChatEvent, ConversationSummary, UsageTotals};
use tokio_util::sync::CancellationToken;

use super::AgentRuntime;
use crate::error::AgentError;
use crate::projection;
use crate::records::{ConversationRecord, Entry};

impl AgentRuntime {
    pub(crate) async fn summary(&self, record: &ConversationRecord) -> ConversationSummary {
        let settings = self.inner.store.settings().await.ok();
        let advisor_default = settings
            .as_ref()
            .is_some_and(|settings| settings.advisor_enabled);
        let effective_model = if let Some(settings) = settings {
            crate::turn::conversation_chain_with_settings(
                self,
                record,
                &settings.for_folder(&record.worker_fp, &record.cwd),
            )
            .await
            .ok()
            .and_then(|chain| chain.into_iter().next())
            .map(|candidate| roost_protocol::wire::agent_chat::ModelRef {
                provider: candidate.info.provider,
                model_id: candidate.info.id,
            })
        } else {
            None
        };
        ConversationSummary {
            id: record.id.clone(),
            title: record.title.clone(),
            worker_fp: record.worker_fp.clone(),
            worker_label: record.worker_label.clone(),
            cwd: record.cwd.clone(),
            model: record.model.clone(),
            effective_model,
            thinking_level: Some(record.thinking_level.clone()),
            mode: record.mode.as_str().to_owned(),
            parent_id: record.parent_id.clone(),
            agent: record.agent.clone(),
            advisor: record.parent_id.is_none() && record.advisor.unwrap_or(advisor_default),
            run_state: record.run_state,
            error: record.error.clone(),
            created_ms: record.created_ms,
            updated_ms: record.updated_ms,
        }
    }

    pub(crate) async fn publish_summary(&self, record: &ConversationRecord) {
        let summary = self.summary(record).await;
        self.inner.sink.conversation(&summary).await;
    }

    pub(crate) async fn emit(&self, id: &str, events: Vec<ChatEvent>) {
        self.inner.sink.events(id, events).await;
    }

    /// Appends an entry and emits the transcript item it projects to.
    pub(crate) async fn append(&self, id: &str, entry: Entry) -> Result<u64, AgentError> {
        self.append_guarded(id, entry, &[]).await
    }

    pub(crate) async fn append_guarded(
        &self,
        id: &str,
        entry: Entry,
        cancellation: &[&CancellationToken],
    ) -> Result<u64, AgentError> {
        let seq = self.append_fenced(id, &entry, cancellation).await?;
        self.emit_entry(id, seq, &entry).await?;
        Ok(seq)
    }

    /// Emits the transcript item a committed entry projects to.
    pub(crate) async fn emit_entry(
        &self,
        id: &str,
        seq: u64,
        entry: &Entry,
    ) -> Result<(), AgentError> {
        let tools = if matches!(entry, Entry::ToolResult { .. }) {
            projection::index_tool_calls(&self.inner.store.entries(id).await?)
        } else {
            projection::ToolCallIndex::new()
        };
        if let Some(item) = projection::entry_item(seq, entry, &tools) {
            self.emit(id, vec![ChatEvent::Item { item }]).await;
        }
        Ok(())
    }

    pub(crate) async fn notice(&self, id: &str, level: &str, title: &str, body: &str) {
        let entry = Entry::Notice {
            level: level.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
        };
        if let Err(error) = self.append(id, entry).await {
            tracing::warn!(conversation_id = id, %error, "agent notice was not recorded");
        }
    }

    /// Adds usage from a model call that has no assistant entry of its own.
    pub(crate) async fn add_extra_usage(
        &self,
        id: &str,
        provider: &str,
        model_id: &str,
        usage: &roost_llm::Usage,
    ) {
        let entry = Entry::ExtraUsage {
            provider: provider.to_owned(),
            model_id: model_id.to_owned(),
            usage: *usage,
        };
        if let Err(error) = self.append_fenced(id, &entry, &[]).await {
            tracing::warn!(conversation_id = id, %error, "out-of-turn usage was not recorded");
        }
        if let Ok(totals) = self.usage_totals(id).await {
            self.emit(id, vec![ChatEvent::Usage { usage: totals }])
                .await;
        }
    }

    pub(crate) async fn usage_totals(&self, id: &str) -> Result<UsageTotals, AgentError> {
        let entries = self.inner.store.entries(id).await?;
        Ok(projection::usage_totals(
            self.inner.llm.catalog(),
            &entries,
            &UsageTotals::default(),
        ))
    }
}
