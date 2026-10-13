//! Durable interrupted-run admission.
//! Every loop snapshot restores before run startup; `runs` rechecks its journal
//! predicate under the lifecycle lock before it starts a task.

use std::collections::{HashMap, HashSet};

use roost_llm::AssistantBlock;
use roost_protocol::wire::agent_chat::AgentRunState;
use tokio_util::sync::CancellationToken;

use super::AgentRuntime;
use crate::error::AgentError;
use crate::model_call::ModelOutput;
use crate::records::{ConversationRecord, Entry};
use crate::roles::ResolvedModel;

impl AgentRuntime {
    /// Reconciles uncertain tools and starts a durable interrupted intent once.
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
        let record = self.record(id).await?;
        let entries = self.inner.store.entries(id).await?;
        if !needs_recovery(&record, &entries) {
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

pub(super) fn recovered_run(
    runtime: &AgentRuntime,
    id: &str,
    entries: &[(u64, Entry)],
) -> Option<crate::run_loop::RecoveredRun> {
    let effective = crate::journal::effective_entries(entries);
    let (assistant_seq, entry) = effective.iter().rev().find_map(|(sequence, entry)| {
        matches!(entry, Entry::Assistant { .. }).then_some((*sequence, entry))
    })?;
    let active_before = entries.iter().rev().find_map(|(sequence, entry)| {
        if *sequence >= assistant_seq {
            return None;
        }
        match entry {
            Entry::RunIntent { active, .. } => Some(*active),
            _ => None,
        }
    })?;
    if !active_before
        || entries.iter().any(|(sequence, entry)| {
            *sequence > assistant_seq && matches!(entry, Entry::RunIntent { active: false, .. })
        })
    {
        return None;
    }
    let Entry::Assistant {
        blocks,
        usage,
        model,
        metadata: Some(metadata),
        ..
    } = entry
    else {
        return None;
    };
    let has_late_submission = latest_submission_seq(entries)
        .is_some_and(|sequence| sequence > metadata.input_through_seq);
    let has_late_user = effective.iter().any(|(sequence, entry)| {
        *sequence > metadata.input_through_seq && matches!(entry, Entry::User { .. })
    });
    let loop_body = runtime
        .inner
        .loops
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(id)
        .and_then(|state| state.body.clone());
    let mut continued = metadata.continued && !has_late_user;
    let mut yield_reminders = metadata.yield_reminders;
    for (sequence, entry) in effective
        .iter()
        .filter(|(sequence, _)| *sequence > assistant_seq)
    {
        let Entry::User { text, .. } = entry else {
            continue;
        };
        let steered_user = entries.iter().any(|(intent_seq, entry)| {
            matches!(
                entry,
                Entry::RunIntent {
                    active: true,
                    through_seq,
                    ..
                } if through_seq == sequence && intent_seq > sequence
            )
        });
        if steered_user {
            continued = false;
        } else if text == crate::prompts::UNEXPECTED_STOP {
            continued = true;
        } else if let Some(attempt) = crate::run_loop::yield_reminder_attempt(text) {
            yield_reminders = yield_reminders.max(attempt);
        } else if loop_body.as_deref() != Some(text.as_str()) {
            continued = false;
        }
    }
    let pending_text = if metadata.completed
        && !has_late_submission
        && metadata.stop_reason.is_some()
        && !blocks
            .iter()
            .any(|block| matches!(block, AssistantBlock::ToolCall { .. }))
    {
        let model = model.as_ref().and_then(|reference| {
            runtime
                .catalog()
                .get(&reference.provider, &reference.model_id)
                .cloned()
        });
        Some(crate::run_loop::RecoveredTextTurn {
            output: ModelOutput {
                blocks: blocks.clone(),
                usage: *usage,
                stop: metadata.stop_reason.clone()?,
            },
            model: model.map(|info| ResolvedModel {
                info,
                thinking: None,
            }),
        })
    } else {
        None
    };
    Some(crate::run_loop::RecoveredRun {
        continued,
        yield_reminders,
        pending_text,
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
fn recovery_depth(
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
