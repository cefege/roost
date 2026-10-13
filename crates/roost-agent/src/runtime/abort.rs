//! Durable user aborts for a conversation and its descendants.
//! `runs` owns each task's cancellation token; this module records the abort
//! boundary first so a process restart cannot reinterpret it as unfinished work.

use std::collections::{HashMap, HashSet};

use crate::error::AgentError;
use crate::records::{ConversationRecord, Entry};
use crate::runtime::AgentRuntime;
use roost_protocol::wire::agent_chat::AgentRunState;

impl AgentRuntime {
    /// Cancels the run tree after its stopped state is durable.
    pub async fn abort(&self, id: &str) -> Result<bool, AgentError> {
        let records = self.inner.store.conversations().await?;
        let Some(root) = self.inner.store.conversation(id).await? else {
            return Ok(false);
        };
        let mut doomed = vec![root.clone()];
        let mut seen = HashSet::from([root.id.clone()]);
        let mut cursor = 0;
        while let Some(parent_id) = doomed.get(cursor).map(|record| record.id.clone()) {
            for child in records
                .iter()
                .filter(|record| record.parent_id.as_deref() == Some(parent_id.as_str()))
            {
                if seen.insert(child.id.clone()) {
                    doomed.push(child.clone());
                }
            }
            cursor += 1;
        }

        let mut lock_ids: Vec<String> = doomed.iter().map(|record| record.id.clone()).collect();
        lock_ids.sort();
        let locks: Vec<_> = lock_ids
            .iter()
            .map(|conversation_id| self.lifecycle_lock(conversation_id))
            .collect();
        let _guards =
            futures::future::join_all(locks.iter().map(|lifecycle| lifecycle.lock())).await;
        let active = self.has_active_work(&doomed).await?;
        if !active {
            return Ok(false);
        }

        let mut first_error = None;
        let mut abort_sequences = HashMap::new();
        for record in &doomed {
            match self.inner.store.entries(&record.id).await {
                Ok(entries) => {
                    let through_seq = entries.last().map_or(0, |(sequence, _)| *sequence);
                    match self
                        .append(
                            &record.id,
                            Entry::RunIntent {
                                active: false,
                                through_seq,
                                aborted: true,
                            },
                        )
                        .await
                    {
                        Ok(sequence) => {
                            abort_sequences.insert(record.id.clone(), sequence);
                        }
                        Err(error) => {
                            first_error.get_or_insert(error);
                        }
                    }
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        for record in &doomed {
            self.mark_aborted(&record.id, true);
            let _ = self.with_run(&record.id, |run| {
                if let Some(sequence) = abort_sequences.get(&record.id) {
                    run.abort_seq
                        .store(*sequence, std::sync::atomic::Ordering::SeqCst);
                }
                run.user_aborted
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                run.cancel.cancel();
            });
        }
        for record in &doomed {
            if let Err(error) = self.pause_loop(&record.id).await {
                first_error.get_or_insert(error);
            }
            if let Err(error) = self.pause_goal_for_abort(&record.id).await {
                first_error.get_or_insert(error);
            }
        }
        tracing::info!(
            conversation_id = id,
            affected = doomed.len(),
            "agent run tree aborted by user"
        );
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(true)
    }

    async fn has_active_work(&self, records: &[ConversationRecord]) -> Result<bool, AgentError> {
        for record in records {
            if self.is_running(&record.id) || record.run_state == AgentRunState::Running {
                return Ok(true);
            }
            let entries = self.inner.store.entries(&record.id).await?;
            let intent = entries.iter().rev().find_map(|(_, entry)| match entry {
                Entry::RunIntent {
                    active,
                    through_seq,
                    aborted,
                } => Some((*active, *through_seq, *aborted)),
                _ => None,
            });
            if let Some((active, through_seq, aborted)) = intent {
                let pending = entries.iter().rev().find_map(|(sequence, entry)| {
                    matches!(entry, Entry::User { .. } | Entry::Advisory { .. })
                        .then_some(*sequence)
                });
                if active || pending.is_some_and(|sequence| sequence > through_seq) {
                    return Ok(true);
                }
                if aborted {
                    continue;
                }
            } else if record.run_state == AgentRunState::Failed {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
