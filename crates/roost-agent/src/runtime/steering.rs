//! Admission and delivery of accepted user or advisor messages.
//! The journal commits input, run intent and the request's dedup row in one
//! transaction before the live queue sees them; any replica may admit, and the
//! one that owns the run is woken to deliver.

use std::sync::atomic::Ordering;

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
        let accepted_submission = matches!(&message, Steer::User(_) | Steer::Advisory { .. });
        let user_submission = matches!(&message, Steer::User(_));
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
        for (seq, appended_entry) in seqs.iter().zip(appended.iter()) {
            self.emit_entry(id, *seq, appended_entry).await?;
        }
        let sequence = seqs
            .get(appended.len().saturating_sub(1))
            .copied()
            .ok_or_else(|| AgentError::Store("admission returned no sequence".into()))?;
        if user_submission && let Err(error) = self.commit_loop_prompt_capture(id, sequence).await {
            self.schedule_readmission(id);
            return Err(error);
        }
        tracing::info!(
            conversation_id = id,
            request_id = command.request_id,
            kind = command.kind,
            sequence,
            "agent command admitted"
        );
        let committed = Steer::Persisted { entry, sequence };
        let queued = self.with_run(id, |run| {
            if run.cancel.is_cancelled() || run.steer.send(committed).is_err() {
                return false;
            }
            run.queued_seq.fetch_max(sequence, Ordering::SeqCst);
            true
        });
        if queued.is_some() {
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
}
