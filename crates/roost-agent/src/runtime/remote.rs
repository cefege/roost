//! Reactions to work another replica committed: input admitted elsewhere is
//! handed to this replica's live run, an abort written elsewhere stops it, and
//! a run whose owner vanished is adopted once its lease expires.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use super::{AgentRuntime, Steer, now_ms};
use crate::error::AgentError;
use crate::journal;
use crate::records::{ConversationRecord, Entry};

/// What a woken replica found for its live run.
enum LocalRun {
    /// No run here; the journal decides whether to start one.
    Absent,
    /// The live run took the change.
    Delivered,
}

impl AgentRuntime {
    /// Applies a change another replica committed to `id`. Idempotent: it
    /// re-reads the journal and only acts on what the live run has not seen.
    pub async fn apply_remote_change(&self, id: &str) {
        match self.deliver_to_local_run(id).await {
            Ok(LocalRun::Delivered) => {}
            Ok(LocalRun::Absent) => {
                if let Err(error) = self.recover_run(id, None).await
                    && !matches!(error, AgentError::NotFound(_))
                {
                    tracing::warn!(conversation_id = id, %error, "remote agent input could not start a run");
                }
            }
            Err(error) => {
                tracing::warn!(conversation_id = id, %error, "remote agent change could not be applied");
            }
        }
    }

    /// Re-reads every live run's journal after notices may have been lost.
    pub async fn resync_local_runs(&self) {
        let live: Vec<String> = super::lock(&self.inner.runs).keys().cloned().collect();
        for id in live {
            self.apply_remote_change(&id).await;
        }
    }

    async fn deliver_to_local_run(&self, id: &str) -> Result<LocalRun, AgentError> {
        let lifecycle = self.lifecycle_lock(id);
        let _guard = lifecycle.lock().await;
        let Some((start_intent, floor, cancelled)) = self.with_run(id, |run| {
            (
                run.start_intent_seq.load(Ordering::SeqCst),
                run.queued_seq
                    .load(Ordering::SeqCst)
                    .max(run.processed_seq.load(Ordering::SeqCst)),
                run.cancel.is_cancelled(),
            )
        }) else {
            return Ok(LocalRun::Absent);
        };
        if self.inner.store.conversation(id).await?.is_none() {
            let _ = self.with_run(id, |run| run.cancel.cancel());
            tracing::info!(
                conversation_id = id,
                "agent run stopped: conversation deleted elsewhere"
            );
            return Ok(LocalRun::Delivered);
        }
        let entries = self.inner.store.entries(id).await?;
        let remote_abort = entries
            .iter()
            .rev()
            .find_map(|(sequence, entry)| match entry {
                Entry::RunIntent {
                    aborted, active, ..
                } => (*aborted && !*active && *sequence > start_intent).then_some(*sequence),
                _ => None,
            });
        if let Some(abort_seq) = remote_abort {
            if !cancelled {
                self.mark_aborted(id, true);
                let _ = self.with_run(id, |run| {
                    run.abort_seq.store(abort_seq, Ordering::SeqCst);
                    run.user_aborted.store(true, Ordering::SeqCst);
                    run.cancel.cancel();
                });
                drop(_guard);
                if let Err(error) = self.pause_loop(id).await {
                    tracing::warn!(conversation_id = id, %error, "loop was not paused after a remote abort");
                }
                tracing::info!(
                    conversation_id = id,
                    abort_seq,
                    "agent run aborted by another coordinator"
                );
            }
            return Ok(LocalRun::Delivered);
        }
        for (sequence, entry) in journal::effective_entries(&entries) {
            let input = matches!(
                entry,
                Entry::User { .. }
                    | Entry::Advisory {
                        delivered: true,
                        ..
                    }
            );
            if !input || sequence <= floor {
                continue;
            }
            let sent = self.with_run(id, |run| {
                let sent = run.steer.send(Steer::Persisted { entry, sequence }).is_ok();
                if sent {
                    run.queued_seq.fetch_max(sequence, Ordering::SeqCst);
                }
                sent
            });
            if sent != Some(true) {
                break;
            }
            tracing::info!(
                conversation_id = id,
                sequence,
                "remote agent input delivered to the live run"
            );
        }
        Ok(LocalRun::Delivered)
    }

    /// Adopts runs whose owner stopped renewing: each is claimed, checked
    /// against its journal, and resumed or released. Parents go first so a
    /// child's run nests under its parent's cancellation.
    pub async fn recover_orphaned_runs(&self) -> Result<usize, AgentError> {
        let orphans = self.inner.store.orphaned_runs(now_ms()).await?;
        if orphans.is_empty() {
            return Ok(0);
        }
        let records: HashMap<String, ConversationRecord> = self
            .inner
            .store
            .conversations()
            .await?
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect();
        let mut candidates: Vec<&ConversationRecord> = orphans
            .iter()
            .filter(|id| !self.is_running(id))
            .filter_map(|id| records.get(id))
            .collect();
        candidates.sort_by_key(|record| super::recovery::recovery_depth(record, &records));
        let mut recovered = 0;
        for record in candidates {
            if let Err(error) = self.restore_loop(&record.id).await {
                tracing::warn!(conversation_id = %record.id, %error, "orphaned run loop state was not restored");
                continue;
            }
            let parent_cancel = record
                .parent_id
                .as_deref()
                .and_then(|parent_id| self.with_run(parent_id, |run| run.cancel.clone()));
            match self.recover_run(&record.id, parent_cancel.as_ref()).await {
                Ok(true) => recovered += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(conversation_id = %record.id, %error, "orphaned agent run was not adopted");
                }
            }
        }
        if recovered > 0 {
            tracing::info!(recovered, instance_id = %self.inner.config.instance_id, "orphaned agent runs adopted");
        }
        Ok(recovered)
    }
}
