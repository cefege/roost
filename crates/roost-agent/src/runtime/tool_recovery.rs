//! Repair interrupted tool rounds before a recovered attempt calls the model.
//! The run supervisor calls this against the effective durable journal; known
//! results are retained, and unknown effects are reported rather than replayed.

use roost_llm::AssistantBlock;

use super::AgentRuntime;
use crate::error::AgentError;
use crate::records::Entry;

const UNKNOWN_OUTCOME: &str = "The harness was interrupted before this tool's result was durably recorded. Its side effects are unknown and it may still be running on the worker. Do not blindly repeat it: inspect current files, processes, and external state first, then continue the existing task from the recorded progress.";

impl AgentRuntime {
    pub(crate) async fn reconcile_interrupted_tools(&self, id: &str) -> Result<(), AgentError> {
        let entries = crate::journal::effective_entries(&self.inner.store.entries(id).await?);
        let mut pending = Vec::new();
        for (_, entry) in &entries {
            match entry {
                Entry::Assistant { blocks, .. } => {
                    for block in blocks {
                        if let AssistantBlock::ToolCall { call_id, name, .. } = block {
                            pending.push((call_id.clone(), name.clone()));
                        }
                    }
                }
                Entry::ToolResult { call_id, .. } => {
                    pending.retain(|(pending_id, _)| pending_id != call_id);
                }
                _ => {}
            }
        }
        for (call_id, tool_name) in pending {
            let children = if tool_name == crate::toolset::TOOL_TASK {
                self.inner
                    .store
                    .conversations()
                    .await?
                    .into_iter()
                    .filter(|record| record.parent_id.as_deref() == Some(id))
                    .map(|record| record.id)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let text = if children.is_empty() {
                UNKNOWN_OUTCOME.to_owned()
            } else {
                format!(
                    "{UNKNOWN_OUTCOME}\nExisting child conversations (do not respawn): {}",
                    children.join(", ")
                )
            };
            self.append(
                id,
                Entry::ToolResult {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    text,
                    is_error: true,
                    details_json: serde_json::json!({
                        "interrupted": true,
                        "outcome_unknown": true,
                        "children": children,
                    })
                    .to_string(),
                },
            )
            .await?;
            tracing::warn!(
                conversation_id = id,
                call_id,
                tool = tool_name,
                "interrupted agent tool outcome recorded without replay"
            );
        }
        Ok(())
    }
}
