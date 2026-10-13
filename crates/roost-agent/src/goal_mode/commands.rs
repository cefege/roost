//! `/goal` command actions and the model's explicit lifecycle tool.
//! Mutations are serialized per conversation and only `complete` marks success.
//! Committed snapshots drive both transcript controls and later recovery.

use roost_protocol::wire::agent_chat::GoalStatus;

use crate::error::AgentError;
use crate::records::ConversationRecord;
use crate::runtime::{AgentRuntime, now_ms, random_id};

use super::describe;

impl AgentRuntime {
    pub(crate) async fn goal_tool(
        &self,
        id: &str,
        expected_goal_id: Option<&str>,
        operation: &str,
        objective: Option<&str>,
        budget: Option<u64>,
    ) -> Result<String, AgentError> {
        if budget == Some(0) {
            return Err(AgentError::InvalidArgument(
                "Goal token budget must be a positive integer.".into(),
            ));
        }
        match operation {
            "create" => {
                let state = self
                    .set_goal(id, objective.unwrap_or_default(), false, budget)
                    .await?;
                Ok(format!("Goal set: {}", state.objective))
            }
            "get" => Ok(self
                .refresh_goal(id)
                .await?
                .map_or_else(|| "No goal set.".into(), |state| describe(&state))),
            "complete" => {
                self.checkpoint_goal_clock(id).await?;
                let goal_lock = self.goal_lock(id);
                let _guard = goal_lock.lock().await;
                let mut state = self
                    .refresh_goal_locked(id)
                    .await?
                    .ok_or_else(|| AgentError::FailedPrecondition("No goal is set.".into()))?;
                ensure_goal_version(expected_goal_id, &state)?;
                if matches!(state.status, GoalStatus::Dropped | GoalStatus::Complete) {
                    return Err(AgentError::FailedPrecondition(
                        "The goal is no longer active.".into(),
                    ));
                }
                state.status = GoalStatus::Complete;
                state.updated_ms = now_ms();
                state.waiting_reason = None;
                state.report = objective.map(str::to_owned);
                self.save_goal(id, state.clone()).await?;
                self.stop_goal_clock(id);
                Ok(format!("Goal completed. {}", describe(&state)))
            }
            "resume" => {
                let lifecycle = self.lifecycle_lock(id);
                let lifecycle_guard = lifecycle.lock().await;
                let goal_lock = self.goal_lock(id);
                let guard = goal_lock.lock().await;
                let mut state = self
                    .refresh_goal_locked(id)
                    .await?
                    .ok_or_else(|| AgentError::FailedPrecondition("No goal is set.".into()))?;
                ensure_goal_version(expected_goal_id, &state)?;
                if !matches!(state.status, GoalStatus::Paused | GoalStatus::BudgetLimited) {
                    return Err(AgentError::FailedPrecondition(
                        "Only a paused or budget-limited goal can be resumed.".into(),
                    ));
                }
                state.status = if state
                    .token_budget
                    .is_some_and(|limit| state.tokens_used >= limit)
                {
                    GoalStatus::BudgetLimited
                } else {
                    GoalStatus::Active
                };
                state.waiting_reason = None;
                state.updated_ms = now_ms();
                self.save_goal(id, state.clone()).await?;
                if state.status == GoalStatus::Active {
                    self.note_goal_resume(id, state.id.clone()).await?;
                }
                if matches!(state.status, GoalStatus::Active | GoalStatus::BudgetLimited)
                    && self.is_running(id)
                {
                    self.start_goal_clock(id).await?;
                }
                drop(guard);
                drop(lifecycle_guard);
                if matches!(state.status, GoalStatus::Active | GoalStatus::BudgetLimited) {
                    self.schedule_goal_continuation(id).await?;
                }
                Ok(format!("Goal resumed: {}", state.objective))
            }
            "drop" => {
                self.checkpoint_goal_clock(id).await?;
                let goal_lock = self.goal_lock(id);
                let _guard = goal_lock.lock().await;
                let mut state = self
                    .refresh_goal_locked(id)
                    .await?
                    .ok_or_else(|| AgentError::FailedPrecondition("No goal is set.".into()))?;
                ensure_goal_version(expected_goal_id, &state)?;
                if matches!(state.status, GoalStatus::Complete | GoalStatus::Dropped) {
                    return Err(AgentError::FailedPrecondition(
                        "The goal is no longer active.".into(),
                    ));
                }
                state.status = GoalStatus::Dropped;
                state.updated_ms = now_ms();
                state.waiting_reason = None;
                self.save_goal(id, state).await?;
                self.stop_goal_clock(id);
                Ok("Goal dropped.".into())
            }
            _ => Err(AgentError::InvalidArgument(
                "goal operation must be create, get, complete, resume, or drop".into(),
            )),
        }
    }

    pub(crate) async fn goal_command(
        &self,
        record: &ConversationRecord,
        args: &str,
    ) -> Result<(), AgentError> {
        let (command, rest) = first_word(args);
        let current = self.refresh_goal(&record.id).await?;
        let operation = match command.to_ascii_lowercase().as_str() {
            "set" => Some("set"),
            "show" => Some("show"),
            "pause" => Some("pause"),
            "resume" => Some("resume"),
            "drop" => Some("drop"),
            "budget" => Some("budget"),
            _ => None,
        };
        match operation.unwrap_or("objective") {
            "show" => {
                self.notice(&record.id, "info", "Goal", &current.as_ref().map_or_else(|| "No goal set.".into(), describe)).await;
                Ok(())
            }
            "objective" | "set" => {
                let objective = if operation.is_none() { args.trim() } else { rest.trim() };
                if objective.is_empty() {
                    self.notice(&record.id, "info", "Goal", &current.as_ref().map_or_else(|| "No goal set.".into(), describe)).await;
                    return Ok(());
                }
                let state = self.set_goal(&record.id, objective, operation == Some("set"), None).await?;
                self.deliver_user_text(record, objective.to_owned()).await?;
                self.notice(&record.id, "info", "Goal set", &state.objective).await;
                Ok(())
            }
            "pause" => self.change_goal_status(record, GoalStatus::Paused).await,
            "resume" => self.change_goal_status(record, GoalStatus::Active).await,
            "drop" => self.change_goal_status(record, GoalStatus::Dropped).await,
            "budget" => self.change_budget(record, rest).await,
            _ => Err(AgentError::InvalidArgument("Use /goal [objective], set <objective>, show, pause, resume, drop, or budget <positive integer|off>.".into())),
        }
    }

    async fn change_goal_status(
        &self,
        record: &ConversationRecord,
        status: GoalStatus,
    ) -> Result<(), AgentError> {
        if status != GoalStatus::Active {
            self.checkpoint_goal_clock(&record.id).await?;
        }
        let lifecycle = self.lifecycle_lock(&record.id);
        let lifecycle_guard = lifecycle.lock().await;
        let goal_lock = self.goal_lock(&record.id);
        let guard = goal_lock.lock().await;
        let mut state = self
            .refresh_goal_locked(&record.id)
            .await?
            .ok_or_else(|| AgentError::FailedPrecondition("No goal is set.".into()))?;
        if matches!(state.status, GoalStatus::Complete | GoalStatus::Dropped) {
            return Err(AgentError::FailedPrecondition(
                "The goal is no longer active.".into(),
            ));
        }
        if status == GoalStatus::Active && state.status != GoalStatus::Paused {
            return Err(AgentError::FailedPrecondition(
                "Only a paused goal can be resumed.".into(),
            ));
        }
        let status = if status == GoalStatus::Active
            && state
                .token_budget
                .is_some_and(|limit| state.tokens_used >= limit)
        {
            GoalStatus::BudgetLimited
        } else {
            status
        };
        state.status = status;
        state.waiting_reason = match status {
            GoalStatus::Paused => Some("Paused by the user.".into()),
            GoalStatus::BudgetLimited => {
                Some("Token budget reached; provide more budget to continue.".into())
            }
            _ => None,
        };
        state.updated_ms = now_ms();
        self.save_goal(&record.id, state.clone()).await?;
        if status == GoalStatus::Active {
            self.note_goal_resume(&record.id, state.id.clone()).await?;
        }
        if matches!(status, GoalStatus::Active | GoalStatus::BudgetLimited) {
            if self.is_running(&record.id) {
                self.start_goal_clock(&record.id).await?;
            }
        } else {
            self.stop_goal_clock(&record.id);
        }
        let title = match status {
            GoalStatus::Paused => "Goal paused",
            GoalStatus::Dropped => "Goal dropped",
            _ => "Goal resumed",
        };
        self.notice(&record.id, "info", title, &state.objective)
            .await;
        drop(guard);
        drop(lifecycle_guard);
        if matches!(status, GoalStatus::Active | GoalStatus::BudgetLimited) {
            self.schedule_goal_continuation(&record.id).await?;
        }
        Ok(())
    }

    async fn change_budget(
        &self,
        record: &ConversationRecord,
        value: &str,
    ) -> Result<(), AgentError> {
        let lifecycle = self.lifecycle_lock(&record.id);
        let lifecycle_guard = lifecycle.lock().await;
        let goal_lock = self.goal_lock(&record.id);
        let guard = goal_lock.lock().await;
        let mut state = self
            .refresh_goal_locked(&record.id)
            .await?
            .ok_or_else(|| AgentError::FailedPrecondition("No goal is set.".into()))?;
        if state.status == GoalStatus::Paused {
            return Err(AgentError::FailedPrecondition(
                "Resume the goal before changing its budget.".into(),
            ));
        }
        if matches!(state.status, GoalStatus::Complete | GoalStatus::Dropped) {
            return Err(AgentError::FailedPrecondition(
                "The goal is no longer active.".into(),
            ));
        }
        state.token_budget = if value.trim().eq_ignore_ascii_case("off") {
            None
        } else {
            Some(
                value
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|budget| *budget > 0)
                    .ok_or_else(|| {
                        AgentError::InvalidArgument(
                            "Goal budget must be a positive integer or off.".into(),
                        )
                    })?,
            )
        };
        state.status = if state
            .token_budget
            .is_some_and(|budget| state.tokens_used >= budget)
        {
            GoalStatus::BudgetLimited
        } else {
            GoalStatus::Active
        };
        state.waiting_reason = (state.status == GoalStatus::BudgetLimited)
            .then(|| "Token budget reached; provide more budget to continue.".into());
        state.updated_ms = now_ms();
        self.save_goal(&record.id, state.clone()).await?;
        self.append(
            &record.id,
            crate::records::Entry::GoalActivity {
                goal_id: state.id.clone(),
                fingerprint: format!("budget-rearm:{}", random_id("budget-")),
            },
        )
        .await?;
        if state.status == GoalStatus::Active && self.is_running(&record.id) {
            self.start_goal_clock(&record.id).await?;
        }
        self.notice(&record.id, "info", "Goal budget updated", &describe(&state))
            .await;
        drop(guard);
        drop(lifecycle_guard);
        if matches!(state.status, GoalStatus::Active | GoalStatus::BudgetLimited) {
            self.schedule_goal_continuation(&record.id).await?;
        }
        Ok(())
    }
}
fn ensure_goal_version(
    expected: Option<&str>,
    state: &roost_protocol::wire::agent_chat::GoalState,
) -> Result<(), AgentError> {
    if expected == Some(state.id.as_str()) {
        Ok(())
    } else {
        Err(AgentError::FailedPrecondition(
            "The goal changed after this model turn began; reload current goal state.".into(),
        ))
    }
}

fn first_word(text: &str) -> (&str, &str) {
    let text = text.trim();
    text.find(char::is_whitespace)
        .map_or((text, ""), |index| (&text[..index], text[index..].trim()))
}
