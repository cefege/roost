//! Durable interrupted-run admission.
//! Every loop snapshot restores before run startup; `runs` rechecks its journal
//! predicate under the lifecycle lock before it starts a task.

use std::collections::{HashMap, HashSet};

use roost_protocol::wire::agent_chat::AgentRunState;
use tokio_util::sync::CancellationToken;

use super::AgentRuntime;
use super::recovered_turn::recovered_run;
use crate::error::AgentError;
use crate::records::{ConversationRecord, Entry};

impl AgentRuntime {
    /// Claims the run lease, then reconciles uncertain tools and starts a
    /// durable interrupted intent once. A lease another replica holds means
    /// that replica is running it; a claimed lease with nothing to recover is
    /// released again.
    pub(crate) async fn recover_run(
        &self,
        id: &str,
        parent: Option<&CancellationToken>,
    ) -> Result<bool, AgentError> {
        let lifecycle = self.lifecycle_lock(id);
        let _guard = lifecycle.lock().await;
        if self.is_running(id) {
            return Ok(false);
        }
        if self.claim_lease(id).await?.is_none() {
            return Ok(false);
        }
        let resumed = self.recover_claimed_run(id, parent).await;
        if !matches!(resumed, Ok(true)) && !self.is_running(id) {
            self.release_lease(id).await;
        }
        resumed
    }

    async fn recover_claimed_run(
        &self,
        id: &str,
        parent: Option<&CancellationToken>,
    ) -> Result<bool, AgentError> {
        let record = self.record(id).await?;
        let entries = self.inner.store.entries(id).await?;
        if !needs_recovery(&record, &entries) {
            if record.run_state == AgentRunState::Running
                && latest_run_intent(&entries).is_some_and(|(active, _, _)| !active)
            {
                self.update_record(id, |row| {
                    row.run_state = AgentRunState::Idle;
                    row.error = None;
                })
                .await?;
            }
            return Ok(false);
        }
        let recovered_run = recovered_run(self, id, &entries);
        self.reconcile_interrupted_tools(id).await?;
        self.append(
            id,
            Entry::Notice {
                level: "warn".into(),
                title: "Run recovered".into(),
                body: "Accepted unfinished work resumed after an interruption".into(),
            },
        )
        .await?;
        let Some(done) = self.start_run_now(id, parent, recovered_run).await? else {
            return Ok(false);
        };
        drop(done);
        tracing::info!(conversation_id = id, "interrupted agent run resumed");
        Ok(true)
    }

    /// Restores every loop snapshot, then resumes accepted unfinished runs.
    pub async fn recover_interrupted_runs(&self) -> Result<usize, AgentError> {
        let records = self.inner.store.conversations().await?;
        let mut restored_loops = HashSet::new();
        let mut entries_by_id = HashMap::new();
        let mut first_error: Option<AgentError> = None;
        for record in &records {
            let live = self.is_running(&record.id);
            let mut loop_ready = live;
            if !live {
                match self.restore_loop(&record.id).await {
                    Ok(()) => loop_ready = true,
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
            }
            let entries = match self.inner.store.entries(&record.id).await {
                Ok(entries) => entries,
                Err(error) => {
                    first_error.get_or_insert(error);
                    continue;
                }
            };
            if latest_run_intent(&entries).is_some_and(|(active, _, aborted)| !active && aborted) {
                self.mark_aborted(&record.id, true);
                if let Err(error) = self.pause_loop(&record.id).await {
                    first_error.get_or_insert(error);
                    loop_ready = false;
                }
            }
            if loop_ready {
                restored_loops.insert(record.id.clone());
            }
            entries_by_id.insert(record.id.clone(), entries);
        }

        let records_by_id: HashMap<String, ConversationRecord> = records
            .iter()
            .map(|record| (record.id.clone(), record.clone()))
            .collect();
        let mut candidates = Vec::new();
        for record in &records {
            let Some(entries) = entries_by_id.get(&record.id) else {
                continue;
            };
            if needs_recovery(record, entries) {
                if restored_loops.contains(&record.id) {
                    candidates.push(record.clone());
                }
                continue;
            }
            if latest_run_intent(entries).is_some_and(|(active, _, _)| !active)
                && record.run_state != AgentRunState::Idle
                && let Err(error) = self
                    .update_record(&record.id, |row| {
                        row.run_state = AgentRunState::Idle;
                        row.error = None;
                    })
                    .await
            {
                first_error.get_or_insert(error);
            }
        }

        candidates.sort_by_key(|record| recovery_depth(record, &records_by_id));
        let mut recovered = 0;
        for record in candidates {
            let parent_cancel = record
                .parent_id
                .as_deref()
                .and_then(|parent_id| self.with_run(parent_id, |run| run.cancel.clone()));
            match self.recover_run(&record.id, parent_cancel.as_ref()).await {
                Ok(true) => recovered += 1,
                Ok(false) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        for record in &records {
            if self.is_running(&record.id)
                || record.mode != crate::records::Mode::Normal
                || self.loop_is_enabled(&record.id)
            {
                continue;
            }
            let Some(entries) = entries_by_id.get(&record.id) else {
                continue;
            };
            if needs_recovery(record, entries) {
                continue;
            }
            let Some(state) = entries.iter().rev().find_map(|(_, entry)| match entry {
                Entry::Goal { state } => Some(state),
                _ => None,
            }) else {
                continue;
            };
            if !matches!(
                state.status,
                roost_protocol::wire::agent_chat::GoalStatus::Active
                    | roost_protocol::wire::agent_chat::GoalStatus::BudgetLimited
            ) || (state.status == roost_protocol::wire::agent_chat::GoalStatus::Active
                && state.waiting_reason.is_some())
            {
                continue;
            }
            match self.schedule_goal_continuation(&record.id).await {
                Ok(()) if self.is_running(&record.id) => recovered += 1,
                Ok(()) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        if recovered > 0 {
            tracing::info!(recovered, "interrupted agent runs resumed");
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(recovered)
    }
}

pub(crate) fn needs_recovery(record: &ConversationRecord, entries: &[(u64, Entry)]) -> bool {
    if let Some((active, through_seq, _)) = latest_run_intent(entries) {
        return active
            || latest_submission_seq(entries).is_some_and(|sequence| sequence > through_seq);
    }
    if matches!(
        record.run_state,
        AgentRunState::Running | AgentRunState::Failed
    ) {
        return true;
    }
    if has_legacy_interruption(entries) {
        return true;
    }
    let latest_assistant = entries.iter().rev().find_map(|(sequence, entry)| {
        matches!(entry, Entry::Assistant { .. }).then_some(*sequence)
    });
    latest_submission_seq(entries)
        .is_some_and(|sequence| latest_assistant.is_none_or(|assistant| sequence > assistant))
}

fn latest_run_intent(entries: &[(u64, Entry)]) -> Option<(bool, u64, bool)> {
    entries.iter().rev().find_map(|(_, entry)| match entry {
        Entry::RunIntent {
            active,
            through_seq,
            aborted,
        } => Some((*active, *through_seq, *aborted)),
        _ => None,
    })
}

pub(crate) fn latest_active_intent_seq(entries: &[(u64, Entry)]) -> Option<u64> {
    entries.iter().rev().find_map(|(sequence, entry)| {
        matches!(entry, Entry::RunIntent { active: true, .. }).then_some(*sequence)
    })
}

pub(crate) fn latest_submission_seq(entries: &[(u64, Entry)]) -> Option<u64> {
    crate::journal::effective_entries(entries)
        .iter()
        .rev()
        .find_map(|(sequence, entry)| {
            matches!(
                entry,
                Entry::User { .. }
                    | Entry::Advisory {
                        delivered: true,
                        ..
                    }
            )
            .then_some(*sequence)
        })
}

fn has_legacy_interruption(entries: &[(u64, Entry)]) -> bool {
    let entries = crate::journal::effective_entries(entries);
    entries.iter().rev().any(|(notice_seq, entry)| {
        matches!(
            entry,
            Entry::Notice {
                title,
                body,
                ..
            } if title == "Run interrupted"
                && body == "Run interrupted by a coordinator restart"
        ) && entries
            .iter()
            .any(|(sequence, entry)| sequence < notice_seq && matches!(entry, Entry::User { .. }))
            && !entries.iter().any(|(sequence, entry)| {
                sequence > notice_seq && matches!(entry, Entry::Assistant { .. })
            })
    })
}
pub(super) fn recovery_depth(
    record: &ConversationRecord,
    records: &HashMap<String, ConversationRecord>,
) -> usize {
    let mut depth = 0;
    let mut parent = record.parent_id.as_deref();
    let mut visited = HashSet::new();
    while let Some(parent_id) = parent {
        if !visited.insert(parent_id) {
            break;
        }
        let Some(parent_record) = records.get(parent_id) else {
            break;
        };
        depth += 1;
        parent = parent_record.parent_id.as_deref();
    }
    depth
}
