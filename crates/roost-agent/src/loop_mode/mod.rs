//! Native `/loop`: after each run ends, repeats the conversation's prompt until
//! a count, a duration, a `--while`/`--until` shell gate, or the user stops it.
//! Its durable state lives in the conversation journal; the run task is the
//! repeat driver while a coordinator is available (see `iteration`).

mod capture;
mod condition;
mod iteration;
mod parse;
mod persistence;
mod state;

use std::collections::HashMap;
use std::sync::{MutexGuard, PoisonError};
use std::time::Instant;

use roost_protocol::wire::agent_chat::{ChatEvent, TranscriptItem};
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::runtime::{AgentRuntime, random_id};
use parse::{ParsedLoop, parse_loop_args};
pub(crate) use state::LoopState;
use state::{Budget, LoopCheck};

/// The one status item the client upserts while a loop is enabled.
pub(crate) const LOOP_STATUS_ITEM_ID: &str = "loop-status";
const LOOP_TITLE: &str = "Loop";
const LOOP_OFF: &str = "Loop mode is off.";
const PROMPT_NEEDS_IDLE: &str = "Stop the current run before starting a loop with a prompt.";
const CHECK_IN_FLIGHT: &str =
    "Wait for the loop condition check to finish before changing the loop.";

impl AgentRuntime {
    /// Handles `/loop [args]`. Bare toggles, `off` and `status` are controls;
    /// anything else configures a loop and enables it.
    pub(crate) async fn run_loop_command(&self, id: &str, args: &str) -> Result<(), AgentError> {
        match args.trim() {
            "" => {
                if self.loop_is_enabled(id) {
                    self.end_loop(id, None).await?;
                    Ok(())
                } else {
                    self.enable_loop(id, ParsedLoop::default()).await
                }
            }
            "off" => {
                if !self.end_loop(id, None).await? {
                    self.notice(id, "info", LOOP_TITLE, LOOP_OFF).await;
                }
                Ok(())
            }
            "status" => {
                let body = self
                    .with_loop(id, |state| state.status_text(Instant::now()))
                    .unwrap_or_else(|| LOOP_OFF.to_owned());
                self.notice(id, "info", LOOP_TITLE, &body).await;
                Ok(())
            }
            other => {
                let parsed = parse_loop_args(other).map_err(AgentError::InvalidArgument)?;
                self.enable_loop(id, parsed).await
            }
        }
    }

    /// Enables a loop, or reconfigures the one already enabled. Reconfiguring
    /// keeps the captured prompt unless a new one is given.
    async fn enable_loop(&self, id: &str, parsed: ParsedLoop) -> Result<(), AgentError> {
        let lifecycle = self.lifecycle_lock(id);
        let lifecycle_guard = lifecycle.lock().await;
        if self.current_goal(id).await?.is_some_and(|goal| {
            matches!(
                goal.status,
                roost_protocol::wire::agent_chat::GoalStatus::Active
                    | roost_protocol::wire::agent_chat::GoalStatus::Paused
                    | roost_protocol::wire::agent_chat::GoalStatus::BudgetLimited
            )
        }) {
            return Err(AgentError::FailedPrecondition(
                "/loop cannot run while a goal is active or paused".into(),
            ));
        }
        let has_prompt = !parsed.prompt.is_empty();
        if has_prompt && self.is_running(id) {
            return Err(AgentError::FailedPrecondition(PROMPT_NEEDS_IDLE.to_owned()));
        }
        let budget = Budget::new(parsed.limit, Instant::now())
            .map_err(|message| AgentError::InvalidArgument(message.to_owned()))?;
        let expected = {
            let mut loops = self.loops();
            if loops.get(id).is_some_and(|state| state.checking.is_some()) {
                return Err(AgentError::FailedPrecondition(CHECK_IN_FLIGHT.to_owned()));
            }
            match loops.get_mut(id) {
                Some(state) => {
                    state.ended.cancel();
                    state.ended = CancellationToken::new();
                    state.budget = budget;
                    state.condition = parsed.condition;
                    state.pending_repeat = None;
                    state.pending_capture = None;
                    state.condition_pending = false;
                    state.condition_ready = false;
                }
                None => {
                    loops.insert(id.to_owned(), LoopState::new(budget, parsed.condition));
                }
            }
            loops.get(id).map(|state| state.ended.clone())
        };
        let Some(expected) = expected else {
            return Ok(());
        };
        if !self.persist_loop_state(id, Some(&expected)).await? {
            return Ok(());
        }
        tracing::info!(conversation_id = id, has_prompt, "loop mode configured");
        drop(lifecycle_guard);
        if has_prompt {
            let record = self.record(id).await?;
            self.deliver_user_text(&record, parsed.prompt).await
        } else {
            self.emit_loop_status(id).await;
            Ok(())
        }
    }

    /// Removes the loop and its status item without a notice, as the user's own
    /// `off` does. Returns whether a loop existed.
    async fn end_loop(
        &self,
        id: &str,
        expected: Option<&CancellationToken>,
    ) -> Result<bool, AgentError> {
        if expected.is_some_and(CancellationToken::is_cancelled) || !self.loop_is_enabled(id) {
            return Ok(false);
        }
        if !self.persist_loop_off(id, expected).await? {
            return Ok(false);
        }
        if !self.take_loop(id, expected) {
            return Ok(false);
        }
        tracing::info!(conversation_id = id, "loop mode stopped");
        // A reset is the only way to remove an item the client has upserted.
        match self.transcript(id).await {
            Ok(transcript) => self.emit(id, vec![ChatEvent::Reset { transcript }]).await,
            Err(error) => {
                tracing::warn!(conversation_id = id, %error, "loop status was not cleared");
            }
        }
        Ok(true)
    }

    /// Ends the loop because the loop itself reached an end (a halt, a failure
    /// or a spent budget), and says why.
    pub(crate) async fn stop_loop(
        &self,
        id: &str,
        expected: Option<&CancellationToken>,
        level: &str,
        message: &str,
    ) {
        match self.end_loop(id, expected).await {
            Ok(true) => self.notice(id, level, LOOP_TITLE, message).await,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(conversation_id = id, %error, "loop stop was not persisted");
            }
        }
    }

    /// Removes the loop from the table and cancels everything working for it:
    /// the condition check, and a repeat prompt still being recorded.
    fn take_loop(&self, id: &str, expected: Option<&CancellationToken>) -> bool {
        let mut loops = self.loops();
        if expected.is_some_and(CancellationToken::is_cancelled) {
            return false;
        }
        // Removal and token cancellation share the lock, so stale work cannot
        // mutate a replacement loop while the old token still looks current.
        let Some(state) = loops.remove(id) else {
            return false;
        };
        state.ended.cancel();
        if let Some(check) = &state.checking {
            check.token.cancel();
        }
        true
    }

    /// Stops the repeats of an aborted run but keeps the loop configured, so a
    /// later prompt resumes it.
    pub(crate) async fn pause_loop(&self, id: &str) -> Result<(), AgentError> {
        let Some(expected) = self.with_loop(id, |state| state.ended.clone()) else {
            return Ok(());
        };
        if self
            .update_loop_state(id, Some(&expected), |state| {
                if let Some(check) = state.checking.take() {
                    check.token.cancel();
                }
                state.body = None;
                state.paused = true;
                state.condition_ready = false;
            })
            .await?
        {
            tracing::info!(conversation_id = id, "loop mode paused");
            self.emit_loop_status(id).await;
        }
        Ok(())
    }

    /// Drops a deleted conversation's loop without emitting anything.
    pub(crate) fn forget_loop(&self, id: &str) {
        self.take_loop(id, None);
    }

    /// Makes a condition check the loop's `checking` slot. Returns `None` when
    /// the loop is gone, so no check starts for it.
    fn begin_check(
        &self,
        id: &str,
        cancel: &CancellationToken,
        expected: &CancellationToken,
    ) -> Option<LoopCheck> {
        let check = LoopCheck {
            id: random_id("loop-check-"),
            token: cancel.child_token(),
        };
        self.with_loop_for(id, expected, |state| state.checking = Some(check.clone()))
            .map(|()| check)
    }

    /// Whether `check_id` still owns the loop's `checking` slot. A check whose
    /// loop was stopped, paused or replaced must not dispatch its shell call.
    fn check_is_current(&self, id: &str, check_id: &str) -> bool {
        self.loops()
            .get(id)
            .and_then(|state| state.checking.as_ref())
            .is_some_and(|check| check.id == check_id)
    }

    /// Clears the `checking` slot if `check_id` still owns it. Returns false when
    /// the loop was stopped, paused or replaced meanwhile: whoever did that owns
    /// the loop from then on.
    async fn end_check(&self, id: &str, check_id: &str) -> bool {
        let cleared = self
            .with_loop(id, |state| {
                if state
                    .checking
                    .as_ref()
                    .is_some_and(|check| check.id == check_id)
                {
                    state.checking = None;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if cleared {
            self.emit_loop_status(id).await;
        }
        cleared
    }

    /// The status item a transcript carries while a loop is enabled.
    pub(crate) fn loop_status_item(&self, id: &str) -> Option<TranscriptItem> {
        let status = self.with_loop(id, |state| state.status_text(Instant::now()))?;
        Some(TranscriptItem::LoopStatus {
            id: LOOP_STATUS_ITEM_ID.to_owned(),
            status,
        })
    }

    pub(crate) async fn emit_loop_status(&self, id: &str) {
        if let Some(item) = self.loop_status_item(id) {
            self.emit(id, vec![ChatEvent::Item { item }]).await;
        }
    }

    pub(crate) fn loop_is_enabled(&self, id: &str) -> bool {
        self.loops().contains_key(id)
    }

    fn loops(&self) -> MutexGuard<'_, HashMap<String, LoopState>> {
        self.inner
            .loops
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn with_loop<T>(&self, id: &str, change: impl FnOnce(&mut LoopState) -> T) -> Option<T> {
        self.loops().get_mut(id).map(change)
    }

    fn with_loop_for<T>(
        &self,
        id: &str,
        expected: &CancellationToken,
        change: impl FnOnce(&mut LoopState) -> T,
    ) -> Option<T> {
        self.with_loop(id, |state| {
            (!expected.is_cancelled()).then(|| change(state))
        })
        .flatten()
    }
}
