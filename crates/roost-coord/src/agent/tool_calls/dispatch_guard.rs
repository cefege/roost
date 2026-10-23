//! Revalidates persisted tool policy after the coordinator's worker wait.

use roost_agent::{AgentRuntime, OwnerConversationId};

pub(super) async fn allows(
    runtime: Option<&AgentRuntime>,
    owner_conversation_id: &OwnerConversationId,
    tool: &str,
    args: &str,
) -> Result<bool, String> {
    let runtime = runtime
        .ok_or_else(|| "agent runtime is unavailable for tool dispatch policy".to_owned())?;
    runtime
        .worker_tool_allowed(&owner_conversation_id.0, tool, args)
        .await
        .map_err(|error| format!("could not revalidate read-only policy: {error}"))
}
