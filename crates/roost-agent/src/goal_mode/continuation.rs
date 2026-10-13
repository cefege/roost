//! Hidden goal continuations, user-input rearming, and progress guards.
//! Each continuation is durable control context, never a visible user bubble.
//! Runtime recovery and run completion share this admission policy.

use roost_protocol::wire::agent_chat::{GoalStatus, TodoStatus};

use crate::error::AgentError;
use crate::records::Entry;
use crate::runtime::{AgentRuntime, now_ms, random_id};

impl AgentRuntime {
    pub(crate) async fn pause_goal_for_abort(&self, id: &str) -> Result<(), AgentError> {
        self.checkpoint_goal_clock(id).await?;
        let goal_lock = self.goal_lock(id);
        let _guard = goal_lock.lock().await;
        let Some(mut state) = self.refresh_goal_locked(id).await? else {
            self.stop_goal_clock(id);
            return Ok(());
        };
        if matches!(state.status, GoalStatus::Active | GoalStatus::BudgetLimited) {
            state.status = GoalStatus::Paused;
            state.waiting_reason = Some("Paused by Stop.".into());
            state.updated_ms = now_ms();
            self.save_goal(id, state).await?;
        }
        self.stop_goal_clock(id);
        Ok(())
    }

    pub(crate) async fn note_goal_user_input(&self, id: &str) -> Result<(), AgentError> {
        let goal_lock = self.goal_lock(id);
        let _guard = goal_lock.lock().await;
        let Some(mut state) = self.refresh_goal_locked(id).await? else {
            return Ok(());
        };
        if state.status == GoalStatus::Active {
            if state.waiting_reason.take().is_some() {
                state.updated_ms = now_ms();
                self.save_goal(id, state.clone()).await?;
            }
            self.append(
                id,
                Entry::GoalActivity {
                    goal_id: state.id,
                    fingerprint: format!("user-input-rearm:{}", random_id("input-")),
                },
            )
            .await?;
        }
        Ok(())
    }

    pub(super) async fn note_goal_resume(
        &self,
        id: &str,
        goal_id: String,
    ) -> Result<(), AgentError> {
        self.append(
            id,
            Entry::GoalActivity {
                goal_id,
                fingerprint: format!("resume-rearm:{}", random_id("resume-")),
            },
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn continue_goal_after_turn(
        &self,
        id: &str,
        steer_rx: &tokio::sync::mpsc::UnboundedReceiver<crate::runtime::Steer>,
    ) -> Result<bool, AgentError> {
        let lifecycle = self.lifecycle_lock(id);
        let _lifecycle_guard = lifecycle.lock().await;
        if !steer_rx.is_empty() {
            return Ok(true);
        }
        let goal_lock = self.goal_lock(id);
        let _goal_guard = goal_lock.lock().await;
        let Some(mut state) = self.refresh_goal_locked(id).await? else {
            return Ok(false);
        };
        if state.status == GoalStatus::BudgetLimited {
            let entries = crate::journal::effective_entries(&self.inner.store.entries(id).await?);
            let marker_seq = entries.iter().rev().find_map(|(seq, entry)| match entry {
                Entry::GoalContinuation {
                    goal_id,
                    budget_wrap_up: true,
                    ..
                } if goal_id == &state.id => Some(*seq),
                _ => None,
            });
            let rearmed = marker_seq.is_some_and(|marker_seq| entries.iter().any(|(seq, entry)| {
                *seq > marker_seq && matches!(entry, Entry::GoalActivity { goal_id, fingerprint } if goal_id == &state.id && fingerprint.starts_with("budget-rearm:"))
            }));
            if marker_seq.is_some() && !rearmed {
                return Ok(false);
            }
            self.append(
                id,
                Entry::GoalContinuation {
                    goal_id: state.id,
                    budget_wrap_up: true,
                    input_through_seq: crate::runtime::latest_submission_seq(&entries).unwrap_or(0),
                },
            )
            .await?;
            return Ok(true);
        }
        if state.status != GoalStatus::Active || state.waiting_reason.is_some() {
            return Ok(false);
        }
        let entries = crate::journal::effective_entries(&self.inner.store.entries(id).await?);
        let start_seq = entries
            .iter()
            .find_map(|(seq, entry)| match entry {
                Entry::Goal { state: candidate } if candidate.id == state.id => Some(*seq),
                _ => None,
            })
            .unwrap_or(0);
        let continuation_seq = entries
            .iter()
            .rev()
            .find_map(|(seq, entry)| match entry {
                Entry::GoalContinuation { goal_id, .. } if goal_id == &state.id => Some(*seq),
                _ => None,
            })
            .unwrap_or(start_seq);
        let explicitly_rearmed = entries.iter().any(|(seq, entry)| {
            *seq > continuation_seq
                && matches!(entry, Entry::GoalActivity { goal_id, fingerprint }
                    if goal_id == &state.id && (fingerprint.starts_with("budget-rearm:")
                        || fingerprint.starts_with("resume-rearm:")))
        });
        if let Some(phases) = entries.iter().rev().find_map(|(_, entry)| match entry {
            Entry::Todo { phases } => Some(phases),
            _ => None,
        }) {
            let actionable = phases
                .iter()
                .flat_map(|phase| &phase.tasks)
                .any(|task| matches!(task.status, TodoStatus::Pending | TodoStatus::InProgress));
            let blocked = phases
                .iter()
                .flat_map(|phase| &phase.tasks)
                .any(|task| task.status == TodoStatus::Blocked);
            if !actionable && blocked {
                state.waiting_reason =
                    Some("Only blocked todo tasks remain; user input is needed.".into());
                state.updated_ms = now_ms();
                self.save_goal(id, state).await?;
                return Ok(false);
            }
        }
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let mut has_activity = false;
        for (_, entry) in entries.iter().filter(|(seq, _)| *seq > continuation_seq) {
            match entry {
                Entry::Assistant { blocks, .. } => {
                    for block in blocks {
                        if let roost_llm::AssistantBlock::ToolCall {
                            name, args_json, ..
                        } = block
                        {
                            has_activity = true;
                            "call".hash(&mut hasher);
                            name.hash(&mut hasher);
                            args_json.hash(&mut hasher);
                        }
                    }
                }
                Entry::ToolResult {
                    tool_name,
                    text,
                    is_error,
                    ..
                } => {
                    has_activity = true;
                    "result".hash(&mut hasher);
                    tool_name.hash(&mut hasher);
                    is_error.hash(&mut hasher);
                    text.hash(&mut hasher);
                }
                _ => {}
            }
        }
        if !has_activity && !explicitly_rearmed {
            state.waiting_reason =
                Some("No tool activity advanced the goal; waiting for user input.".into());
            state.updated_ms = now_ms();
            self.save_goal(id, state).await?;
            return Ok(false);
        }
        if has_activity {
            let fingerprint = format!("{:016x}", hasher.finish());
            let previous = entries.iter().rev().find_map(|(_, entry)| match entry {
                Entry::GoalActivity {
                    goal_id,
                    fingerprint,
                } if goal_id == &state.id => Some(fingerprint.as_str()),
                _ => None,
            });
            if previous == Some(fingerprint.as_str()) {
                state.waiting_reason = Some(
                    "Repeated tool activity made no new progress; waiting for user input.".into(),
                );
                state.updated_ms = now_ms();
                self.save_goal(id, state).await?;
                return Ok(false);
            }
            self.append(
                id,
                Entry::GoalActivity {
                    goal_id: state.id.clone(),
                    fingerprint,
                },
            )
            .await?;
        }
        self.append(
            id,
            Entry::GoalContinuation {
                goal_id: state.id,
                budget_wrap_up: false,
                input_through_seq: crate::runtime::latest_submission_seq(&entries).unwrap_or(0),
            },
        )
        .await?;
        Ok(true)
    }

    // Goal tools can resume a run from inside its task; erase the recursive spawn future.
    pub(crate) fn schedule_goal_continuation<'a>(
        &'a self,
        id: &'a str,
    ) -> futures::future::BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            let lifecycle = self.lifecycle_lock(id);
            let _lifecycle_guard = lifecycle.lock().await;
            if self.is_running(id) {
                return Ok(());
            }
            let goal_lock = self.goal_lock(id);
            let _goal_guard = goal_lock.lock().await;
            let Some(state) = self.refresh_goal_locked(id).await? else {
                return Ok(());
            };
            let budget_limited = state.status == GoalStatus::BudgetLimited;
            if state.status != GoalStatus::Active && !budget_limited {
                return Ok(());
            }
            if state.status == GoalStatus::Active && state.waiting_reason.is_some() {
                return Ok(());
            }
            let record = self.record(id).await?;
            if record.mode != crate::records::Mode::Normal || self.loop_is_enabled(id) {
                return Ok(());
            }
            let entries = self.inner.store.entries(id).await?;
            let wrap_up_marker = entries.iter().rev().find_map(|(seq, entry)| match entry {
                Entry::GoalContinuation {
                    goal_id,
                    budget_wrap_up: true,
                    ..
                } if goal_id == &state.id => Some(*seq),
                _ => None,
            });
            let rearmed = wrap_up_marker.is_some_and(|marker_seq| entries.iter().any(|(seq, entry)| {
            *seq > marker_seq && matches!(entry, Entry::GoalActivity { goal_id, fingerprint } if goal_id == &state.id && fingerprint.starts_with("budget-rearm:"))
        }));
            if budget_limited {
                if let Some(marker_seq) = wrap_up_marker {
                    if !rearmed
                        && entries.iter().any(|(seq, entry)| {
                            *seq > marker_seq && matches!(entry, Entry::Assistant { .. })
                        })
                    {
                        return Ok(());
                    }
                    if rearmed {
                        self.append(
                            id,
                            Entry::GoalContinuation {
                                goal_id: state.id.clone(),
                                budget_wrap_up: true,
                                input_through_seq: crate::runtime::latest_submission_seq(&entries)
                                    .unwrap_or(0),
                            },
                        )
                        .await?;
                    }
                } else {
                    self.append(
                        id,
                        Entry::GoalContinuation {
                            goal_id: state.id.clone(),
                            budget_wrap_up: true,
                            input_through_seq: crate::runtime::latest_submission_seq(&entries)
                                .unwrap_or(0),
                        },
                    )
                    .await?;
                }
            } else {
                self.append(
                    id,
                    Entry::GoalContinuation {
                        goal_id: state.id.clone(),
                        budget_wrap_up: false,
                        input_through_seq: crate::runtime::latest_submission_seq(&entries)
                            .unwrap_or(0),
                    },
                )
                .await?;
            }
            self.start_goal_clock(id).await?;
            let _ = self.start_run_now(id, None, None).await?;
            Ok(())
        })
    }
}
