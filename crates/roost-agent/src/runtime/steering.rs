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
            None,
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
        mode: Option<super::SubmitMode>,
    ) -> Result<(), AgentError> {
        let lifecycle = self.lifecycle_lock(id);
        let _guard = lifecycle.lock().await;
        let (user_text, user_attachments) = match &message {
            Steer::User(text) => (Some(text.as_str()), &[][..]),
            Steer::UserWithAttachments(text, attachments) => {
                (Some(text.as_str()), attachments.as_slice())
            }
            _ => (None, &[][..]),
        };
        let user_submission = user_text.is_some();
        let journal_delivery = matches!(
            &message,
            Steer::BackgroundJob(_) | Steer::PeerMessage { .. }
        );
        let wakes = message.wakes_idle_conversation();
        let accepted_submission = user_submission
            || matches!(&message, Steer::Advisory { .. })
            || (journal_delivery && wakes);
        if let Some(text) = user_text {
            self.prepare_loop_prompt_capture(id, text).await?;
        }
        let entry = match (user_text, mode) {
            (Some(text), Some(super::SubmitMode::FollowUp)) => Entry::QueuedInput {
                submission_id: command.request_id.to_owned(),
                text: text.to_owned(),
                attachments: user_attachments.to_vec(),
                state: crate::records::QueuedInputState::Pending,
            },
            (Some(text), Some(super::SubmitMode::Steer | super::SubmitMode::Reject)) => {
                Entry::User {
                    text: text.to_owned(),
                    attachments: user_attachments.to_vec(),
                    loop_repeat: false,
                    item_id: Some(command.request_id.to_owned()),
                }
            }
            _ => message.clone().into_entry(),
        };
        let mut entries = prefix;
        entries.push(entry.clone());
        let appended = entries.clone();
        let intent = match mode {
            Some(super::SubmitMode::FollowUp) => IntentChange::FollowUp,
            Some(super::SubmitMode::Reject) => IntentChange::RejectIfBusy,
            Some(super::SubmitMode::Steer) | None if wakes => IntentChange::Activate,
            Some(super::SubmitMode::Steer) | None => IntentChange::None,
        };
        let admitted = self
            .inner
            .store
            .admit_command(
                id,
                CommandAdmission {
                    request_id: command.request_id.to_owned(),
                    kind: command.kind.to_owned(),
                    entries,
                    intent,
                    fence: None,
                    expected_intent_seq: None,
                    completion_through: None,
                },
            )
            .await;
        let (seqs, queued) = match admitted {
            Ok(AdmitOutcome::Admitted { seqs, queued }) => (seqs, queued),
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
            Ok(AdmitOutcome::Busy) => {
                self.cancel_loop_prompt_capture(id).await;
                return Err(AgentError::FailedPrecondition(
                    "conversation is busy".into(),
                ));
            }
            Ok(AdmitOutcome::NotWithdrawable) => {
                self.cancel_loop_prompt_capture(id).await;
                return Err(AgentError::FailedPrecondition(
                    "submission is no longer withdrawable".into(),
                ));
            }
            Ok(AdmitOutcome::Stopped) => {
                self.cancel_loop_prompt_capture(id).await;
                return Err(AgentError::FailedPrecondition(
                    "conversation was stopped before admission".into(),
                ));
            }
            Err(error) => {
                if user_submission {
                    self.cancel_loop_prompt_capture(id).await;
                }
                return Err(error);
            }
        };
        let queue_sequence = seqs
            .get(appended.len().saturating_sub(1))
            .copied()
            .ok_or_else(|| AgentError::Store("admission returned no sequence".into()))?;
        let follow_up_activated = mode == Some(super::SubmitMode::FollowUp) && !queued;
        let sequence = if follow_up_activated {
            seqs.get(appended.len())
                .copied()
                .ok_or_else(|| AgentError::Store("activated follow-up has no user entry".into()))?
        } else {
            queue_sequence
        };
        let joins_live_run = (user_submission || journal_delivery)
            && self
                .with_run(id, |run| !run.cancel.is_cancelled())
                .unwrap_or(false);
        if user_submission && joins_live_run && mode != Some(super::SubmitMode::FollowUp) {
            self.inner.tools.detach_background_calls(id).await;
        }
        let mut committed_entries = appended.clone();
        if follow_up_activated
            && let Entry::QueuedInput {
                submission_id,
                text,
                attachments,
                ..
            } = &entry
        {
            committed_entries.pop();
            committed_entries.extend([
                Entry::QueuedInput {
                    submission_id: submission_id.clone(),
                    text: text.clone(),
                    attachments: attachments.clone(),
                    state: crate::records::QueuedInputState::Activated,
                },
                Entry::User {
                    text: text.clone(),
                    attachments: attachments.clone(),
                    loop_repeat: false,
                    item_id: Some(submission_id.clone()),
                },
            ]);
            committed_entries.push(Entry::RunIntent {
                active: true,
                through_seq: sequence,
                aborted: false,
                yielded: false,
            });
        }
        for (seq, appended_entry) in seqs.iter().zip(committed_entries.iter()) {
            if queued && matches!(appended_entry, Entry::QueuedInput { .. }) {
                if let Entry::QueuedInput {
                    submission_id,
                    text,
                    attachments,
                    ..
                } = appended_entry
                {
                    self.emit(
                        id,
                        vec![ChatEvent::Queued {
                            message: QueuedMessage {
                                id: submission_id.clone(),
                                text: text.clone(),
                                attachment_names: attachments
                                    .iter()
                                    .map(|attachment| attachment.filename.clone())
                                    .collect(),
                                mode: "follow_up".into(),
                            },
                        }],
                    )
                    .await;
                }
            } else if let Entry::User {
                text,
                attachments,
                item_id,
                ..
            } = appended_entry
                && joins_live_run
                && mode != Some(super::SubmitMode::FollowUp)
            {
                self.emit(
                    id,
                    vec![ChatEvent::Queued {
                        message: QueuedMessage {
                            id: item_id
                                .clone()
                                .unwrap_or_else(|| crate::projection::user_item_id(*seq)),
                            text: text.clone(),
                            attachment_names: attachments
                                .iter()
                                .map(|attachment| attachment.filename.clone())
                                .collect(),
                            mode: String::new(),
                        },
                    }],
                )
                .await;
            } else {
                self.emit_entry(id, *seq, appended_entry).await?;
            }
        }
        if user_submission
            && !queued
            && let Err(error) = self.commit_loop_prompt_capture(id, sequence).await
        {
            self.schedule_readmission(id);
            return Err(error);
        }
        tracing::info!(
            conversation_id = id,
            request_id = command.request_id,
            kind = command.kind,
            sequence,
            queued = queued || joins_live_run,
            "agent command admitted"
        );
        self.arrival_signal(id).notify_waiters();
        if queued {
            self.cancel_loop_prompt_capture(id).await;
            return Ok(());
        }
        if mode == Some(super::SubmitMode::FollowUp) {
            if !wakes {
                self.inner.signal.conversation_changed(id).await;
                return Ok(());
            }
            return match self.start_run_now_boxed(id).await {
                Ok(Some(_)) | Ok(None) => Ok(()),
                Err(error) => {
                    self.schedule_readmission(id);
                    Err(error)
                }
            };
        }
        let committed = Steer::Persisted {
            entry: entry.clone(),
            sequence,
        };
        let handed_off = self.with_run(id, |run| {
            if run.cancel.is_cancelled() || run.steer.send(committed).is_err() {
                return false;
            }
            run.queued_seq.fetch_max(sequence, Ordering::SeqCst);
            true
        });
        if handed_off == Some(true) {
            return Ok(());
        }
        if joins_live_run {
            self.emit_entry(id, sequence, &entry).await?;
        }
        if handed_off == Some(false) {
            return Ok(());
        }
        if !wakes {
            self.inner.signal.conversation_changed(id).await;
            return Ok(());
        }
        match self.start_run_now_boxed(id).await {
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

    /// Journals a message from a peer conversation and delivers it: into the live
    /// run at its next step boundary, or as a new run when `wakes_idle` and the
    /// recipient is idle.
    pub async fn deliver_peer_message(
        &self,
        id: &str,
        entry: Entry,
        wakes_idle: bool,
    ) -> Result<(), AgentError> {
        self.steer(id, Steer::PeerMessage { entry, wakes_idle })
            .await
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
                && matches!(
                    entry,
                    Entry::User { .. } | Entry::BackgroundJob { .. } | Entry::PeerMessage { .. }
                )
                && let Err(error) = self.emit_entry(id, sequence, &entry).await
            {
                tracing::warn!(conversation_id = id, %error, "unread queued message was not shown");
            }
        }
    }
}
