//! The repeat driver. The run task calls `next_loop_iteration` after each
//! `run_conversation` returns, so the same task starts the next repeat, and a
//! stop, pause, or abort ends the chain without a second task racing it.

use std::time::Instant;

use tokio_util::sync::CancellationToken;

use super::condition::{self, Verdict};
use super::parse::Condition;
use crate::error::AgentError;
use crate::records::Entry;
use crate::runtime::{AgentRuntime, RunEnd};

impl AgentRuntime {
    /// Admits the next repeat after a completed run. Store failures retry under
    /// run supervision; unknown shell-gate outcomes end repetition without replay.
    pub(crate) async fn next_loop_iteration(
        &self,
        id: &str,
        end: &RunEnd,
        user_aborted: bool,
        cancel: &CancellationToken,
    ) -> Result<bool, AgentError> {
        if user_aborted || cancel.is_cancelled() || matches!(end, RunEnd::Aborted) {
            if let Err(error) = self.pause_loop(id).await {
                tracing::warn!(conversation_id = id, %error, "loop pause was not persisted");
            }
            return Ok(false);
        }
        self.reconcile_pending_loop_repeat(id).await?;
        let Some((budget, has_body, condition, condition_pending, condition_ready, expected)) =
            self.with_loop(id, |state| {
                (
                    state.budget,
                    state.body.is_some(),
                    state.condition.clone(),
                    state.condition_pending,
                    state.condition_ready,
                    state.ended.clone(),
                )
            })
        else {
            return Ok(false);
        };
        if !has_body {
            return Ok(false);
        }
        if condition_pending {
            self.stop_loop(
                id,
                Some(&expected),
                "error",
                super::state::CONDITION_INTERRUPTED,
            )
            .await;
            return Ok(false);
        }
        if let Some(message) = budget.exhausted(Instant::now()) {
            self.stop_loop(id, Some(&expected), "info", message).await;
            return Ok(false);
        }
        let gated = if condition_ready {
            true
        } else {
            match condition {
                Some(condition) => self.gate_repeat(id, &condition, cancel, &expected).await?,
                None => true,
            }
        };
        if !gated {
            return Ok(false);
        }
        if cancel.is_cancelled() {
            self.pause_loop(id).await?;
            return Ok(false);
        }
        self.arm_iteration(id, cancel, &expected).await
    }

    /// Runs the `--while`/`--until` gate only after its pending marker commits.
    async fn gate_repeat(
        &self,
        id: &str,
        condition: &Condition,
        cancel: &CancellationToken,
        expected: &CancellationToken,
    ) -> Result<bool, AgentError> {
        let Some(check) = self.begin_check(id, cancel, expected) else {
            return Ok(false);
        };
        match self
            .update_loop_state(id, Some(expected), |state| state.condition_pending = true)
            .await
        {
            Ok(true) => {}
            Ok(false) => return Ok(false),
            Err(error) => {
                self.end_check(id, &check.id).await;
                self.with_loop_for(id, expected, |state| state.condition_pending = false);
                return Err(error);
            }
        }
        self.emit_loop_status(id).await;
        let record = match self.record(id).await {
            Ok(record) => record,
            Err(error) => {
                self.end_check(id, &check.id).await;
                self.with_loop_for(id, expected, |state| state.condition_pending = false);
                return Err(error);
            }
        };
        let verdict = if !check.token.is_cancelled() && self.check_is_current(id, &check.id) {
            condition::evaluate(self, &record, condition, &check).await
        } else {
            Verdict::Cancelled
        };
        if !self.end_check(id, &check.id).await {
            return Ok(false);
        }
        match verdict {
            Verdict::Continue => {
                self.update_loop_state(id, Some(expected), |state| {
                    state.condition_pending = false;
                    state.condition_ready = true;
                })
                .await
            }
            Verdict::Halt(message) => {
                self.stop_loop(id, Some(expected), "info", &message).await;
                Ok(false)
            }
            Verdict::Error(message) => {
                self.stop_loop(
                    id,
                    Some(expected),
                    "error",
                    &format!("Loop stopped: {message}"),
                )
                .await;
                Ok(false)
            }
            Verdict::Cancelled => {
                if cancel.is_cancelled() {
                    self.pause_loop(id).await?;
                }
                Ok(false)
            }
        }
    }

    /// A pending marker makes an accepted repeat count even when the store
    /// loses its acknowledgement or the process exits before saving the budget.
    async fn arm_iteration(
        &self,
        id: &str,
        cancel: &CancellationToken,
        expected: &CancellationToken,
    ) -> Result<bool, AgentError> {
        let Some(body) = self
            .with_loop_for(id, expected, |state| state.body.clone())
            .flatten()
        else {
            return Ok(false);
        };
        let entries = self.inner.store.entries(id).await?;
        let after_seq = entries.last().map_or(0, |(seq, _)| *seq);
        if !self
            .update_loop_state(id, Some(expected), |state| {
                state.pending_repeat = Some(super::state::PendingRepeat {
                    after_seq,
                    body: body.clone(),
                });
            })
            .await?
        {
            return Ok(false);
        }
        let user_seq = match self
            .append_guarded(
                id,
                Entry::User {
                    text: body,
                    loop_repeat: true,
                },
                &[cancel, expected],
            )
            .await
        {
            Ok(sequence) => sequence,
            Err(AgentError::Cancelled) => {
                if cancel.is_cancelled() {
                    self.pause_loop(id).await?;
                }
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        if !self
            .update_loop_state(id, Some(expected), |state| {
                state.budget = state.budget.spend();
                state.pending_repeat = None;
                state.condition_ready = false;
            })
            .await?
        {
            return Ok(false);
        }
        tracing::info!(conversation_id = id, user_seq, "loop repeat started");
        self.emit_loop_status(id).await;
        Ok(true)
    }
}
