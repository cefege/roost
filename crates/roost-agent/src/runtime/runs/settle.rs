//! Settles one owned run, records its terminal boundary, and restarts only for accepted work.
//! `runs.rs` owns the task lifecycle; this module owns the journal transition at its end.

use std::sync::atomic::Ordering;

use roost_protocol::wire::agent_chat::{AgentRunState, ChatEvent};
use tokio::sync::mpsc;

use super::{AgentRuntime, RunEnd, Steer};
use crate::error::AgentError;
use crate::records::Entry;
use crate::traits::{AdmitOutcome, CommandAdmission, IntentChange, PlanAdmission};

impl AgentRuntime {
    pub(super) async fn settle_run(
        &self,
        id: &str,
        end: &RunEnd,
        user_aborted: bool,
        mut steer_rx: mpsc::UnboundedReceiver<Steer>,
    ) {
        let lifecycle = self.lifecycle_lock(id);
        let _guard = lifecycle.lock().await;
        if let Err(error) = self.checkpoint_goal_clock(id).await {
            tracing::warn!(conversation_id = id, %error, "agent run goal clock did not settle");
        }
        self.stop_goal_clock(id);
        let handle = self.take_run(id);
        let processed_seq = handle
            .as_ref()
            .map_or(0, |run| run.processed_seq.load(Ordering::SeqCst));
        let run_start_intent = handle
            .as_ref()
            .map_or(0, |run| run.start_intent_seq.load(Ordering::SeqCst));
        let mut queued_sequences = Vec::new();
        while let Ok(message) = steer_rx.try_recv() {
            if let Steer::Persisted { sequence, .. } = message {
                queued_sequences.push(sequence);
            }
        }
        let (mut entries, mut entries_read_failed) = match self.inner.store.entries(id).await {
            Ok(entries) => (entries, false),
            Err(error) => {
                tracing::warn!(conversation_id = id, %error, "agent run journal could not be read while settling");
                (Vec::new(), true)
            }
        };
        let latest_active = super::super::recovery::latest_active_intent_seq(&entries).unwrap_or(0);
        let superseded_abort = user_aborted
            && handle
                .as_ref()
                .is_some_and(|run| latest_active > run.abort_seq.load(Ordering::SeqCst));
        let mut aborted = (user_aborted || matches!(end, RunEnd::Aborted)) && !superseded_abort;
        self.mark_aborted(id, aborted);
        let last_seq = entries.last().map_or(0, |(sequence, _)| *sequence);
        let through_seq = if aborted { last_seq } else { processed_seq };
        let mut completion_stopped = false;
        let intent_result = if !aborted && matches!(end, RunEnd::Completed) {
            self.inner
                .store
                .admit_command(
                    id,
                    CommandAdmission {
                        request_id: format!("run-complete-{run_start_intent}"),
                        kind: "run-complete".into(),
                        entries: Vec::new(),
                        intent: IntentChange::Complete,
                        plan_admission: PlanAdmission::AllowPending,
                        fence: self.held_lease(id),
                        expected_intent_seq: Some(run_start_intent),
                        completion_through: Some(processed_seq),
                    },
                )
                .await
                .and_then(|outcome| match outcome {
                    AdmitOutcome::Admitted { .. } | AdmitOutcome::Duplicate { .. } => Ok(()),
                    AdmitOutcome::Stopped => {
                        completion_stopped = true;
                        Ok(())
                    }
                    AdmitOutcome::Busy => Err(AgentError::Store(
                        "run completion was unexpectedly rejected as busy".into(),
                    )),
                    AdmitOutcome::PendingPlan | AdmitOutcome::PlanNotProposed => {
                        Err(AgentError::Store(
                            "run completion was unexpectedly rejected by a plan".into(),
                        ))
                    }
                    AdmitOutcome::NotWithdrawable => Err(AgentError::Store(
                        "run completion was unexpectedly treated as a withdrawal".into(),
                    )),
                })
        } else {
            self.append(
                id,
                Entry::RunIntent {
                    active: false,
                    through_seq,
                    aborted,
                    yielded: matches!(end, RunEnd::Yielded(_)),
                },
            )
            .await
            .map(|_| ())
        };
        aborted |= completion_stopped;
        self.mark_aborted(id, aborted);
        let intent_error = intent_result.err();
        if intent_error.is_none() {
            match self.inner.store.entries(id).await {
                Ok(updated_entries) => entries = updated_entries,
                Err(_) => entries_read_failed = true,
            }
        }
        let effective_entries = crate::journal::effective_entries(&entries);
        let consumed_through = entries
            .iter()
            .filter_map(|(_, entry)| match entry {
                Entry::InputConsumed { through_seq } => Some(*through_seq),
                Entry::Assistant {
                    metadata: Some(metadata),
                    ..
                } => Some(metadata.input_through_seq),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let queued = queued_sequences.iter().any(|sequence| {
            effective_entries.iter().any(|(entry_sequence, entry)| {
                entry_sequence == sequence
                    && (!matches!(entry, Entry::User { .. }) || *sequence > consumed_through)
            })
        });
        let pending = crate::runtime::latest_submission_seq(&entries)
            .is_some_and(|sequence| sequence > processed_seq && sequence > consumed_through);
        let plan_pending = crate::admission::has_pending_plan(&entries);
        if matches!(intent_error, Some(AgentError::Fenced(_))) {
            drop(_guard);
            self.abandon_fenced_run(id).await;
            return;
        }
        let (run_state, error) = if let Some(error) = &intent_error {
            (AgentRunState::Failed, Some(error.to_string()))
        } else {
            match end {
                RunEnd::Failed(error) => (AgentRunState::Failed, Some(error.clone())),
                RunEnd::Completed | RunEnd::Yielded(_) | RunEnd::Aborted => {
                    (AgentRunState::Idle, None)
                }
            }
        };
        tracing::info!(conversation_id = id, ?end, "agent run ended");
        if let Err(store_error) = self
            .update_record(id, |record| {
                record.run_state = run_state;
                record.error = error.clone();
            })
            .await
        {
            tracing::warn!(conversation_id = id, %store_error, "agent run state was not saved");
        }
        if let Some(error) = &intent_error {
            tracing::warn!(conversation_id = id, %error, "agent terminal intent was not saved");
        }
        self.emit(id, vec![ChatEvent::RunState { run_state, error }])
            .await;
        let explicit_after_stop = completion_stopped
            && pending
            && super::super::recovery::latest_active_intent_seq(&entries)
                .is_some_and(|sequence| sequence > run_start_intent);
        let restart = !plan_pending
            && (!aborted || explicit_after_stop)
            && !matches!(end, RunEnd::Yielded(_))
            && (pending || queued);
        if !restart {
            self.release_unread_queue(id, &entries, processed_seq).await;
        }
        if intent_error.is_none() {
            self.release_lease(id).await;
        }
        let restart_boundary = entries.iter().rev().find_map(|(sequence, entry)| {
            matches!(entry, Entry::RunIntent { .. }).then_some(*sequence)
        });
        drop(_guard);
        if intent_error.is_some() || (entries_read_failed && !aborted) {
            self.schedule_readmission(id);
        } else if restart
            && let Some(boundary) = restart_boundary
            && let Err(error) = self.start_run(id, None, Some(boundary)).await
        {
            tracing::warn!(conversation_id = id, %error, "accepted late steer did not start another run");
            self.schedule_readmission(id);
        }
    }
}
