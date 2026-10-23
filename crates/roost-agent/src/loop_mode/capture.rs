//! Commit-aware admission of user prompts into an active loop.
//! The journaled User entry is authoritative; this module stages and reconciles
//! loop budget and body updates around that append.

use std::time::Instant;

use crate::error::AgentError;
use crate::records::Entry;
use crate::runtime::AgentRuntime;

impl AgentRuntime {
    /// Stages capture before the User append, so recovery can distinguish a
    /// committed prompt from a failed append without spending the budget early.
    pub(crate) async fn prepare_loop_prompt_capture(
        &self,
        id: &str,
        text: &str,
    ) -> Result<(), AgentError> {
        if self.is_running(id) {
            return Ok(());
        }
        let Some((budget, expected)) =
            self.with_loop(id, |state| (state.budget, state.ended.clone()))
        else {
            return Ok(());
        };
        if let Some(message) = budget.exhausted(Instant::now()) {
            self.stop_loop(id, Some(&expected), "info", message).await;
            return Ok(());
        }
        let entries = self.inner.store.entries(id).await?;
        let after_seq = entries.last().map_or(0, |(seq, _)| *seq);
        self.update_loop_state(id, Some(&expected), |state| {
            state.pending_capture = Some(super::state::PendingCapture {
                after_seq,
                body: text.to_owned(),
            });
        })
        .await?;
        Ok(())
    }

    /// Commits capture accounting only after the User entry is durable.
    pub(crate) async fn commit_loop_prompt_capture(
        &self,
        id: &str,
        user_seq: u64,
    ) -> Result<(), AgentError> {
        let Some(pending) = self
            .with_loop(id, |state| state.pending_capture.clone())
            .flatten()
        else {
            return Ok(());
        };
        if user_seq <= pending.after_seq {
            return Ok(());
        }
        let accepted = self
            .inner
            .store
            .entries(id)
            .await?
            .into_iter()
            .any(|(seq, entry)| {
                seq == user_seq
                    && matches!(entry, Entry::User { text, loop_repeat: false, .. } if text == pending.body)
            });
        if !accepted {
            return Ok(());
        }
        if self
            .update_loop_state(id, None, |state| {
                if state.pending_capture.as_ref().is_some_and(|current| {
                    current.after_seq == pending.after_seq && current.body == pending.body
                }) {
                    state.body = Some(pending.body);
                    state.paused = false;
                    state.condition_ready = false;
                    state.budget = state.budget.spend();
                    state.pending_capture = None;
                }
            })
            .await?
        {
            tracing::info!(conversation_id = id, user_seq, "loop prompt captured");
            self.emit_loop_status(id).await;
        }
        Ok(())
    }

    /// Reconciles a failed append result: an entry may have committed before
    /// storage failed to acknowledge it.
    pub(crate) async fn cancel_loop_prompt_capture(&self, id: &str) {
        let Some(pending) = self
            .with_loop(id, |state| state.pending_capture.clone())
            .flatten()
        else {
            return;
        };
        let entries = match self.inner.store.entries(id).await {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(conversation_id = id, %error, "loop prompt capture outcome was not reconciled");
                return;
            }
        };
        if let Some(user_seq) = entries.into_iter().find_map(|(seq, entry)| {
            (seq > pending.after_seq
                && matches!(entry, Entry::User { text, loop_repeat: false, .. } if text == pending.body))
                .then_some(seq)
        }) {
            if let Err(error) = self.commit_loop_prompt_capture(id, user_seq).await {
                tracing::warn!(conversation_id = id, %error, "committed loop prompt capture was not saved");
            }
            return;
        }
        if let Err(error) = self
            .update_loop_state(id, None, |state| {
                if state.pending_capture.as_ref().is_some_and(|current| {
                    current.after_seq == pending.after_seq && current.body == pending.body
                }) {
                    state.pending_capture = None;
                }
            })
            .await
        {
            tracing::warn!(conversation_id = id, %error, "unaccepted loop prompt capture was not cleared");
        }
    }
}
