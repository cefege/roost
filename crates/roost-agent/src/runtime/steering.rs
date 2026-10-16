//! Admission and delivery of accepted user or advisor messages.
//! The journal commits input, run intent and the request's dedup row in one
//! transaction before the live queue sees them; any replica may admit, and the
//! one that owns the run is woken to deliver.

use std::sync::atomic::Ordering;

use roost_protocol::wire::agent_chat::{ChatEvent, QueuedMessage};

use super::{AgentRuntime, Steer, random_id};
use crate::error::AgentError;
use crate::records::Entry;
use crate::traits::{AdmitOutcome, CommandAdmission, IntentChange};

/// The client request a steer answers, for deduplication of retries.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CommandKey<'a> {
    pub(crate) request_id: &'a str,
    pub(crate) kind: &'a str,
}

impl AgentRuntime {
    /// Delivers a message: into the running loop after its current tool
    /// round, or as a new run when the conversation is idle.
    pub(crate) async fn steer(&self, id: &str, message: Steer) -> Result<(), AgentError> {
        let request_id = random_id("steer-");
        self.steer_command(
            id,
            CommandKey {
                request_id: &request_id,
                kind: "steer",
            },
            Vec::new(),
            message,
        )
        .await
    }

    /// [`Self::steer`] for a client command: `prefix` commits in the same
    /// transaction ahead of the message, and a retried `request_id` is a no-op.
    pub(crate) async fn steer_command(
        &self,
        id: &str,
        command: CommandKey<'_>,
        prefix: Vec<Entry>,
        message: Steer,
    ) -> Result<(), AgentError> {
        let lifecycle = self.lifecycle_lock(id);
        let _guard = lifecycle.lock().await;
        let user_submission = matches!(&message, Steer::User(_));
        let job_delivery = matches!(&message, Steer::BackgroundJob(_));
        let wakes = message.wakes_idle_conversation();
        let accepted_submission =
            matches!(&message, Steer::User(_) | Steer::Advisory { .. }) || (job_delivery && wakes);
        if let Steer::User(text) = &message {
            self.prepare_loop_prompt_capture(id, text).await?;
        }
        let entry = message.into_entry();
        let mut entries = prefix;
        entries.push(entry.clone());
        let appended = entries.clone();
        let admitted = self
            .inner
            .store
            .admit_command(
                id,
                CommandAdmission {
                    request_id: command.request_id.to_owned(),
                    kind: command.kind.to_owned(),
                    entries,
                    intent: IntentChange::Activate,
                },
            )
            .await;
        let seqs = match admitted {
            Ok(AdmitOutcome::Admitted { seqs }) => seqs,
            Ok(AdmitOutcome::Duplicate { .. }) => {
                if user_submission {
                    self.cancel_loop_prompt_capture(id).await;
                }
                tracing::info!(
                    conversation_id = id,
                    request_id = command.request_id,
                    "duplicate agent command ignored"
                );
                return Ok(());
            }
            Err(error) => {
                if user_submission {
                    self.cancel_loop_prompt_capture(id).await;
                }
                return Err(error);
            }
        };
        let sequence = seqs
            .get(appended.len().saturating_sub(1))
            .copied()
            .ok_or_else(|| AgentError::Store("admission returned no sequence".into()))?;
        // A run that is working reads this message at its next boundary; until
        // then it waits in the queue rather than appearing above the reply the
        // run is still writing.
        let joins_live_run = (user_submission || job_delivery)
            && self
                .with_run(id, |run| !run.cancel.is_cancelled())
                .unwrap_or(false);
        if user_submission && joins_live_run {
            self.inner.tools.detach_background_calls(id).await;
        }
        for (seq, appended_entry) in seqs.iter().zip(appended.iter()) {
            match appended_entry {
                Entry::User { text, .. } if joins_live_run && *seq == sequence => {
                    self.emit(
                        id,
                        vec![ChatEvent::Queued {
                            message: QueuedMessage {
                                id: crate::projection::user_item_id(*seq),
                                text: text.clone(),
                            },
                        }],
                    )
                    .await;
                }
                _ => self.emit_entry(id, *seq, appended_entry).await?,
            }
        }
        if user_submission && let Err(error) = self.commit_loop_prompt_capture(id, sequence).await {
            self.schedule_readmission(id);
            return Err(error);
        }
        tracing::info!(
            conversation_id = id,
            request_id = command.request_id,
            kind = command.kind,
            sequence,
            queued = joins_live_run,
            "agent command admitted"
        );
        let committed = Steer::Persisted {
            entry: entry.clone(),
            sequence,
        };
        let queued = self.with_run(id, |run| {
            if run.cancel.is_cancelled() || run.steer.send(committed).is_err() {
                return false;
            }
            run.queued_seq.fetch_max(sequence, Ordering::SeqCst);
            true
        });
        self.arrival_signal(id).notify_waiters();
        if queued == Some(true) {
            return Ok(());
        }
        // The run stopped between the check and the hand-off: show the message
        // now; the next run reads it from the journal.
        if joins_live_run {
            self.emit_entry(id, sequence, &entry).await?;
        }
        if queued == Some(false) {
            // An unwinding run settles under this lock and restarts for it.
            return Ok(());
        }
        if !wakes {
            self.inner.signal.conversation_changed(id).await;
            return Ok(());
        }
        match self.start_run_now(id, None, None).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => {
                self.inner.signal.conversation_changed(id).await;
                Ok(())
            }
            Err(error) => {
                if accepted_submission {
                    self.schedule_readmission(id);
                }
                Err(error)
            }
        }
    }

    /// Journals a settled background job and delivers it like a user message:
    /// into the live run after its current tool round, or as a new run when idle.
    pub async fn deliver_background_job(&self, id: &str, entry: Entry) -> Result<(), AgentError> {
        self.steer(id, Steer::BackgroundJob(entry)).await
    }
}

impl AgentRuntime {
    /// Shows every message queued behind a run that ended without reading
    /// it; the next run reads them from the journal.
    pub(crate) async fn release_unread_queue(
        &self,
        id: &str,
        entries: &[(u64, Entry)],
        read_through: u64,
    ) {
        for (sequence, entry) in crate::journal::effective_entries(entries) {
            if sequence > read_through
                && matches!(entry, Entry::User { .. } | Entry::BackgroundJob { .. })
                && let Err(error) = self.emit_entry(id, sequence, &entry).await
            {
                tracing::warn!(conversation_id = id, %error, "unread queued message was not shown");
            }
        }
    }
}
