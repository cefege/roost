//! Read-only inspection assembled from the durable conversation and effective toolset.
//! The coordinator exposes this snapshot to the agent workspace without secrets.

use std::collections::BTreeMap;

use roost_protocol::wire::agent_chat::{
    AgentActivity, AgentInspection, ContextInspection, InspectedTool,
};

use super::AgentRuntime;
use crate::compaction::{HEADROOM_TOKENS, KEEP_TOKENS};
use crate::error::AgentError;
use crate::journal::effective_entries;
use crate::records::{ConversationRecord, Entry};

fn conversation_depth(record: &ConversationRecord, conversations: &[ConversationRecord]) -> u32 {
    let mut depth = 0;
    let mut parent_id = record.parent_id.as_deref();
    while let Some(parent) = parent_id {
        depth += 1;
        parent_id = conversations
            .iter()
            .find(|candidate| candidate.id == parent)
            .and_then(|candidate| candidate.parent_id.as_deref());
    }
    depth
}

impl AgentRuntime {
    pub async fn inspect(&self, id: &str) -> Result<AgentInspection, AgentError> {
        let record = self.record(id).await?;
        let conversations = self.inner.store.conversations().await?;
        let depth = conversation_depth(&record, &conversations);
        let entries = self.inner.store.entries(id).await?;
        let effective = effective_entries(&entries);
        let summary = self.summary(&record).await;
        let transcript = self.transcript(id).await?;
        let find_available = crate::find::available(self, id).await;
        let toolset = crate::toolset::toolset_for(&record, depth, find_available);
        let lsp_read_only = toolset.lsp_read_only;
        let tools = toolset
            .specs
            .into_iter()
            .map(|spec| {
                let restriction = (lsp_read_only && spec.name == "lsp")
                    .then(|| "Read-only LSP actions in this mode".to_owned());
                InspectedTool {
                    name: spec.name,
                    description: spec.description,
                    parameters: spec.parameters,
                    restriction,
                }
            })
            .collect();
        let model = summary.effective_model.as_ref().or(record.model.as_ref());
        let context_window_tokens = model
            .and_then(|model| self.catalog().get(&model.provider, &model.model_id))
            .map(|model| model.context_window);
        let context_tokens =
            crate::compaction::latest_compaction_usage_tokens(&effective).unwrap_or(0);
        let compaction_trigger_tokens = context_window_tokens
            .filter(|window| *window > 0)
            .map(crate::compaction::compaction_trigger_tokens);
        let last_compacted_seq = effective
            .iter()
            .rev()
            .find_map(|(seq, entry)| matches!(entry, Entry::Compaction { .. }).then_some(*seq));
        let goal = effective.iter().rev().find_map(|(_, entry)| match entry {
            Entry::Goal { state } => Some(state.clone()),
            _ => None,
        });
        let mut activity = Vec::new();
        if transcript.run_state == roost_protocol::wire::agent_chat::AgentRunState::Running {
            activity.push(AgentActivity {
                kind: "run".into(),
                label: "Agent run".into(),
                state: "running".into(),
                detail: None,
            });
        }
        for child in conversations
            .iter()
            .filter(|candidate| candidate.parent_id.as_deref() == Some(id))
        {
            if child.run_state != roost_protocol::wire::agent_chat::AgentRunState::Idle {
                activity.push(AgentActivity {
                    kind: "subagent".into(),
                    label: child.agent.clone().unwrap_or_else(|| "Subagent".into()),
                    state: child.run_state.as_str().to_owned(),
                    detail: Some(child.title.clone()),
                });
            }
        }
        activity.extend(latest_running_jobs(&effective));
        Ok(AgentInspection {
            conversation: summary,
            context: ContextInspection {
                context_tokens,
                context_window_tokens,
                compaction_trigger_tokens,
                compaction_headroom_tokens: HEADROOM_TOKENS,
                compaction_keep_tokens: KEEP_TOKENS,
                last_compacted_seq,
            },
            tools,
            activity,
            read_only: record.read_only,
            goal,
            inbox: transcript.queued.clone(),
            usage: transcript.usage,
        })
    }
}

fn latest_running_jobs(entries: &[(u64, Entry)]) -> Vec<AgentActivity> {
    let mut latest = BTreeMap::new();
    for (_, entry) in entries {
        if let Entry::BackgroundJob { job_id, state, .. } = entry {
            latest.insert(job_id.as_str(), *state);
        }
    }
    latest
        .into_iter()
        .filter(|(_, state)| *state == roost_protocol::wire::agent_chat::JobState::Running)
        .map(|(job_id, state)| AgentActivity {
            kind: "background_job".into(),
            label: "Background job".into(),
            state: state.as_str().to_owned(),
            detail: Some(job_id.to_owned()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::latest_running_jobs;
    use crate::records::Entry;
    use roost_protocol::wire::agent_chat::JobState;

    fn job(job_id: &str, state: JobState) -> Entry {
        Entry::BackgroundJob {
            job_id: job_id.into(),
            command: "command".into(),
            state,
            is_error: state == JobState::Failed,
            output: String::new(),
            exit_code: None,
        }
    }

    #[test]
    fn inspection_reports_only_latest_running_job_state() {
        let old_running = job("finished", JobState::Running);
        let completed = job("finished", JobState::Completed);
        let current = job("current", JobState::Running);
        let entries = [(1, old_running), (2, completed), (3, current)];
        let activity = latest_running_jobs(&entries);
        assert_eq!(activity.len(), 1);
        assert_eq!(activity[0].detail.as_deref(), Some("current"));
        assert_eq!(activity[0].state, "running");
    }
}
