//! The repeat driver. The run task calls `next_loop_iteration` after each
//! `run_conversation` returns, so the same task starts the next repeat, and a
//! stop, pause, or abort ends the chain without a second task racing it.

use std::time::Instant;

use tokio_util::sync::CancellationToken;

use super::condition::{self, Verdict};
use super::parse::Condition;
use crate::records::Entry;
use crate::runtime::{AgentRuntime, RunEnd};

impl AgentRuntime {
    /// Decides whether the run that just ended is followed by a repeat. Returns
    /// `true` once the repeat's prompt is recorded, so the caller runs again.
    pub(crate) async fn next_loop_iteration(
        &self,
        id: &str,
        end: &RunEnd,
        user_aborted: bool,
        cancel: &CancellationToken,
    ) -> bool {
        if user_aborted || cancel.is_cancelled() || matches!(end, RunEnd::Aborted) {
            self.pause_loop(id).await;
            return false;
        }
        if let RunEnd::Failed(error) = end {
            self.stop_loop(
                id,
                None,
                "error",
                &format!("Loop stopped: the run failed. {error}"),
            )
            .await;
            return false;
        }
        let Some((budget, has_body, condition, expected)) = self.with_loop(id, |state| {
            (
                state.budget,
                state.body.is_some(),
                state.condition.clone(),
                state.ended.clone(),
            )
        }) else {
            return false;
        };
        if !has_body {
            return false;
        }
        if let Some(message) = budget.exhausted(Instant::now()) {
            self.stop_loop(id, Some(&expected), "info", message).await;
            return false;
        }
        let gated = match condition {
            Some(condition) => self.gate_repeat(id, &condition, cancel, &expected).await,
            None => true,
        };
        if !gated {
            return false;
        }
        if cancel.is_cancelled() {
            self.pause_loop(id).await;
            return false;
        }
        self.arm_iteration(id, cancel, &expected).await
    }

    /// Runs the `--while`/`--until` gate for the run that just ended. Returns
    /// whether the loop repeats.
    async fn gate_repeat(
        &self,
        id: &str,
        condition: &Condition,
        cancel: &CancellationToken,
        expected: &CancellationToken,
    ) -> bool {
        let Some(check) = self.begin_check(id, cancel, expected) else {
            return false;
        };
        self.emit_loop_status(id).await;
        // The shell call is dispatched only while this check still owns the
        // slot, so a stop, pause or abort during the lookup never starts it.
        let verdict = match self.record(id).await {
            Ok(record) if !check.token.is_cancelled() && self.check_is_current(id, &check.id) => {
                condition::evaluate(self, &record, condition, &check).await
            }
            Ok(_) => Verdict::Cancelled,
            Err(error) => Verdict::Error(error.to_string()),
        };
        // Once the check no longer owns the slot, the stopper already ended or
        // paused the loop and emitted its state.
        if !self.end_check(id, &check.id).await {
            return false;
        }
        match verdict {
            Verdict::Continue => true,
            Verdict::Halt(message) => {
                self.stop_loop(id, Some(expected), "info", &message).await;
                false
            }
            Verdict::Error(message) => {
                self.stop_loop(
                    id,
                    Some(expected),
                    "error",
                    &format!("Loop stopped: {message}"),
                )
                .await;
                false
            }
            Verdict::Cancelled => {
                // A check cancelled by `/loop off` has no state left to pause;
                // only a user abort of the run pauses the loop.
                if cancel.is_cancelled() {
                    self.pause_loop(id).await;
                }
                false
            }
        }
    }

    /// Records the repeat's prompt before spending its run. The store checks
    /// cancellation at its commit boundary; after that boundary an abort is a
    /// started, interrupted repeat, not an unspent repeat.
    async fn arm_iteration(
        &self,
        id: &str,
        cancel: &CancellationToken,
        expected: &CancellationToken,
    ) -> bool {
        let Some(body) = self
            .with_loop_for(id, expected, |state| state.body.clone())
            .flatten()
        else {
            return false;
        };
        // Await the commit result even after cancellation: dropping a database
        // future after COMMIT was sent can leave a prompt with an unspent run.
        match self
            .append_guarded(id, Entry::User { text: body }, &[cancel, expected])
            .await
        {
            Err(crate::error::AgentError::Cancelled) => {
                if cancel.is_cancelled() {
                    self.pause_loop(id).await;
                }
                false
            }
            Err(error) => {
                self.stop_loop(
                    id,
                    Some(expected),
                    "error",
                    &format!("Loop stopped: the repeat prompt was not recorded. {error}"),
                )
                .await;
                false
            }
            Ok(_) => {
                self.with_loop_for(id, expected, |state| state.budget = state.budget.spend());
                tracing::info!(conversation_id = id, "loop repeat started");
                self.emit_loop_status(id).await;
                true
            }
        }
    }
}
