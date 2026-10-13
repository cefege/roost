//! Durable goal snapshots, idempotent usage reconciliation, and live-time checkpoints.
//! Journal sequence numbers bound accounting to one goal identity, while the
//! runtime-only monotonic clock is discarded at process restart.

use roost_protocol::wire::agent_chat::{GoalState, GoalStatus};
use std::time::Instant;

use crate::error::AgentError;
use crate::records::{Entry, Mode};
use crate::runtime::{AgentRuntime, now_ms, random_id};

impl AgentRuntime {
    pub(crate) async fn current_goal(&self, id: &str) -> Result<Option<GoalState>, AgentError> {
        Ok(self.goal_snapshot(id).await?.map(|(state, _)| state))
    }

    pub(crate) async fn refresh_goal(&self, id: &str) -> Result<Option<GoalState>, AgentError> {
        let goal_lock = self.goal_lock(id);
        let _guard = goal_lock.lock().await;
        self.refresh_goal_locked(id).await
    }

    pub(super) async fn refresh_goal_locked(
        &self,
        id: &str,
    ) -> Result<Option<GoalState>, AgentError> {
        let Some((state, changed)) = self.goal_snapshot(id).await? else {
            return Ok(None);
        };
        if changed {
            self.save_goal(id, state.clone()).await?;
        }
        Ok(Some(state))
    }

    async fn goal_snapshot(&self, id: &str) -> Result<Option<(GoalState, bool)>, AgentError> {
        let entries = self.inner.store.entries(id).await?;
        let Some((_, mut state)) = entries.iter().rev().find_map(|(seq, entry)| match entry {
            Entry::Goal { state } => Some((*seq, state.clone())),
            _ => None,
        }) else {
            return Ok(None);
        };
        let baseline = entries
            .iter()
            .find_map(|(_, entry)| match entry {
                Entry::Goal { state: candidate } if candidate.id == state.id => {
                    Some(candidate.tokens_used)
                }
                _ => None,
            })
            .unwrap_or(0);
        let tokens = entries
            .iter()
            .filter_map(|(_, entry)| match entry {
                Entry::Assistant {
                    usage,
                    metadata: Some(metadata),
                    ..
                } if metadata.goal_id.as_deref() == Some(state.id.as_str()) => Some(usage),
                _ => None,
            })
            .fold(baseline, |total, usage| {
                total
                    .saturating_add(usage.input)
                    .saturating_add(usage.output)
                    .saturating_add(usage.cache_write)
            })
            .max(state.tokens_used);
        let mut changed = tokens != state.tokens_used;
        state.tokens_used = tokens;
        if state.status == GoalStatus::Active
            && state.token_budget.is_some_and(|limit| tokens >= limit)
        {
            state.status = GoalStatus::BudgetLimited;
            state.waiting_reason = Some(
                "Token budget reached; complete a concise wrap-up without new substantive work."
                    .into(),
            );
            changed = true;
        }
        if changed {
            state.updated_ms = now_ms();
        }
        Ok(Some((state, changed)))
    }

    pub(super) async fn save_goal(&self, id: &str, state: GoalState) -> Result<(), AgentError> {
        let goal_id = state.id.clone();
        let status = state.status.as_str();
        self.append(id, Entry::Goal { state }).await?;
        tracing::info!(conversation_id = id, %goal_id, status, "goal state changed");
        Ok(())
    }

    pub(super) async fn set_goal(
        &self,
        id: &str,
        objective: &str,
        allow_replace: bool,
        token_budget: Option<u64>,
    ) -> Result<GoalState, AgentError> {
        let lifecycle = self.lifecycle_lock(id);
        let _lifecycle_guard = lifecycle.lock().await;
        let goal_lock = self.goal_lock(id);
        let _goal_guard = goal_lock.lock().await;
        let objective = objective.trim();
        if objective.is_empty() {
            return Err(AgentError::InvalidArgument(
                "/goal requires an objective".into(),
            ));
        }
        let record = self.record(id).await?;
        if record.mode == Mode::Plan {
            return Err(AgentError::FailedPrecondition(
                "/goal cannot run while plan mode is active".into(),
            ));
        }
        if self.loop_is_enabled(id) {
            return Err(AgentError::FailedPrecondition(
                "/goal and /loop cannot run together".into(),
            ));
        }
        if let Some(existing) = self.current_goal(id).await? {
            if existing.status == GoalStatus::Paused {
                return Err(AgentError::FailedPrecondition(
                    "Resume or drop the paused goal before replacing it.".into(),
                ));
            }
            if matches!(
                existing.status,
                GoalStatus::Active | GoalStatus::BudgetLimited
            ) && !allow_replace
            {
                return Err(AgentError::FailedPrecondition("An active goal already exists; use /goal set, /goal show, /goal pause, or /goal drop.".into()));
            }
        }
        let now = now_ms();
        let state = GoalState {
            id: random_id("goal-"),
            objective: objective.to_owned(),
            status: GoalStatus::Active,
            token_budget,
            tokens_used: 0,
            time_used_seconds: 0,
            created_ms: now,
            updated_ms: now,
            report: None,
            waiting_reason: None,
        };
        self.save_goal(id, state.clone()).await?;
        self.stop_goal_clock(id);
        self.start_goal_clock(id).await?;
        Ok(state)
    }

    pub(crate) async fn start_goal_clock(&self, id: &str) -> Result<(), AgentError> {
        let Some(goal) = self.current_goal(id).await? else {
            return Ok(());
        };
        if !matches!(goal.status, GoalStatus::Active | GoalStatus::BudgetLimited) {
            return Ok(());
        }
        let mut clocks = self.goal_clocks();
        if clocks
            .get(id)
            .is_none_or(|(goal_id, _)| goal_id != &goal.id)
        {
            clocks.insert(id.to_owned(), (goal.id, Instant::now()));
        }
        Ok(())
    }

    pub(crate) async fn checkpoint_goal_clock(&self, id: &str) -> Result<(), AgentError> {
        let goal_lock = self.goal_lock(id);
        let _goal_guard = goal_lock.lock().await;
        let Some((goal_id, checkpoint)) = self.goal_clocks().get(id).cloned() else {
            return Ok(());
        };
        let elapsed = checkpoint.elapsed().as_secs();
        if elapsed == 0 {
            return Ok(());
        }
        let Some(mut state) = self.current_goal(id).await? else {
            self.stop_goal_clock(id);
            return Ok(());
        };
        if state.id != goal_id
            || !matches!(state.status, GoalStatus::Active | GoalStatus::BudgetLimited)
        {
            self.stop_goal_clock(id);
            return Ok(());
        }
        state.time_used_seconds = state.time_used_seconds.saturating_add(elapsed);
        state.updated_ms = now_ms();
        self.save_goal(id, state).await?;
        let mut clocks = self.goal_clocks();
        if let Some((current_id, current_checkpoint)) = clocks.get_mut(id)
            && *current_id == goal_id
            && *current_checkpoint == checkpoint
        {
            *current_checkpoint = Instant::now();
        }
        Ok(())
    }

    pub(crate) fn stop_goal_clock(&self, id: &str) {
        self.goal_clocks().remove(id);
    }
}
