//! Durable user aborts for a conversation and its descendants.
//! `runs` owns each task's cancellation token; this module records the abort
//! boundary first so a process restart cannot reinterpret it as unfinished work.

use std::collections::{HashMap, HashSet};

use crate::error::AgentError;
use crate::records::{ConversationRecord, Entry};
use crate::runtime::{AgentRuntime, random_id};
use crate::traits::{AdmitOutcome, CommandAdmission, IntentChange, PlanAdmission};
use roost_protocol::wire::agent_chat::{AgentRunState, ChatEvent};

impl AgentRuntime {
    /// Withdraws an unconsumed queued steer or follow-up exactly once.
    pub async fn withdraw(
        &self,
        id: &str,
        request_id: &str,
        submission_id: &str,
    ) -> Result<bool, AgentError> {
        if request_id.trim().is_empty() || submission_id.trim().is_empty() {
            return Err(AgentError::InvalidArgument(
                "request_id and submission_id are required".into(),
            ));
        }
        let lifecycle = self.lifecycle_lock(id);
        let _guard = lifecycle.lock().await;
        let outcome = self
            .inner
            .store
            .admit_command(
                id,
                CommandAdmission {
                    request_id: request_id.to_owned(),
                    kind: "withdraw".into(),
                    entries: vec![Entry::QueuedInputWithdrawn {
                        submission_id: submission_id.to_owned(),
                    }],
                    intent: IntentChange::Withdraw,
                    plan_admission: PlanAdmission::AllowPending,
                    fence: None,
                    expected_intent_seq: None,
                    completion_through: None,
                },
            )
            .await?;
        match outcome {
            AdmitOutcome::Admitted { .. } => {
                self.emit(
                    id,
                    vec![ChatEvent::QueueRemoved {
                        submission_id: submission_id.to_owned(),
                    }],
                )
                .await;
                self.inner.signal.conversation_changed(id).await;
                Ok(true)
            }
            AdmitOutcome::Duplicate { .. }
            | AdmitOutcome::Busy
            | AdmitOutcome::PendingPlan
            | AdmitOutcome::PlanNotProposed
            | AdmitOutcome::Stopped
            | AdmitOutcome::NotWithdrawable => Ok(false),
        }
    }
}

impl AgentRuntime {
    /// Cancels the run tree after its stopped state is durable.
    pub async fn abort(&self, id: &str) -> Result<bool, AgentError> {
        self.abort_with_background(id, true).await
    }

    /// Cancels foreground work and optionally background jobs.
    pub async fn abort_with_background(
        &self,
        id: &str,
        include_background: bool,
    ) -> Result<bool, AgentError> {
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
                .filter(|record| record.is_child_of(&parent_id))
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
        if include_background {
            for record in &doomed {
                self.inner.tools.abort_background_jobs(&record.id).await;
            }
        }
        let active = self.has_active_work(&doomed).await?;
        if !active {
            return Ok(false);
        }

        let mut first_error = None;
        let mut abort_sequences = HashMap::new();
        for record in &doomed {
            let admitted = self
                .inner
                .store
                .admit_command(
                    &record.id,
                    CommandAdmission {
                        request_id: random_id("abort-"),
                        kind: "abort".into(),
                        entries: Vec::new(),
                        intent: IntentChange::Abort,
                        plan_admission: PlanAdmission::AllowPending,
                        fence: None,
                        expected_intent_seq: None,
                        completion_through: None,
                    },
                )
                .await;
            match admitted {
                Ok(AdmitOutcome::Admitted { seqs, .. }) => {
                    if let Some(sequence) = seqs.last() {
                        abort_sequences.insert(record.id.clone(), *sequence);
                    }
                }
                Ok(
                    AdmitOutcome::Duplicate { .. }
                    | AdmitOutcome::Busy
                    | AdmitOutcome::PendingPlan
                    | AdmitOutcome::PlanNotProposed
                    | AdmitOutcome::Stopped,
                ) => {}
                Ok(AdmitOutcome::NotWithdrawable) => {
                    first_error.get_or_insert(AgentError::Store(
                        "abort admission returned a withdrawal result".to_owned(),
                    ));
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        let mut remote = Vec::new();
        for record in &doomed {
            self.mark_aborted(&record.id, true);
            let local = self.with_run(&record.id, |run| {
                if let Some(sequence) = abort_sequences.get(&record.id) {
                    run.abort_seq
                        .store(*sequence, std::sync::atomic::Ordering::SeqCst);
                }
                run.user_aborted
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                run.cancel.cancel();
            });
            if local.is_none() {
                remote.push(record.id.clone());
            }
        }
        for record in &doomed {
            if let Err(error) = self.pause_loop(&record.id).await {
                first_error.get_or_insert(error);
            }
            if let Err(error) = self.pause_goal_for_abort(&record.id).await {
                first_error.get_or_insert(error);
            }
        }
        for conversation_id in &remote {
            self.inner
                .signal
                .conversation_changed(conversation_id)
                .await;
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
                    ..
                } => Some((*active, *through_seq, *aborted)),
                _ => None,
            });
            if let Some((active, through_seq, aborted)) = intent {
                let pending = entries.iter().rev().find_map(|(sequence, entry)| {
                    let pending_entry = match entry {
                        Entry::User { .. } | Entry::Advisory { .. } => true,
                        // A broadcast is buffered for the recipient's own next turn; only a
                        // direct message is work that must finish before the conversation settles.
                        Entry::PeerMessage { broadcast, .. } => !broadcast,
                        Entry::BackgroundJob { state, .. } => state.wakes_conversation(),
                        _ => false,
                    };
                    pending_entry.then_some(*sequence)
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
