//! Persistence for the conversation's effective read-only tool setting.
//! Coordinator configure RPCs call this; record updates publish the summary.
//! The setting stays independent of normal and Plan agent modes.

use roost_protocol::wire::agent_chat::{
    ConversationSummary, TOOL_CONTEXT_FILES, TOOL_REWIND_FILES,
};

use super::super::AgentRuntime;
use crate::error::AgentError;

impl AgentRuntime {
    pub async fn set_read_only(
        &self,
        id: &str,
        read_only: bool,
    ) -> Result<ConversationSummary, AgentError> {
        let record = self
            .update_record(id, |row| row.read_only = read_only)
            .await?;
        tracing::info!(
            conversation_id = id,
            read_only,
            "agent read-only setting saved"
        );
        self.inner.signal.conversation_summary_changed(id).await;
        Ok(self.summary(&record).await)
    }

    /// Revalidate model tools at dispatch; hidden controller operations remain available.
    pub async fn worker_tool_allowed(
        &self,
        id: &str,
        name: &str,
        args: &str,
    ) -> Result<bool, AgentError> {
        let record = self.record(id).await?;
        Ok(matches!(name, TOOL_CONTEXT_FILES | TOOL_REWIND_FILES)
            || !record.read_only
            || crate::read_only_guard::allows(name, args))
    }
}
