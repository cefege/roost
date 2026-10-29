//! Ported from oh-my-pi packages/coding-agent/src/plan-mode/ (MIT).
//! Plan mode: `/plan` toggles a read-only planning mode on the `plan` role's
//! model; `propose_plan` records a proposal and ends the run; the user's
//! decision approves (here or in a new chat) or asks for refinement.

use roost_protocol::wire::agent_chat::parse_slash_command;

use crate::error::AgentError;
use crate::prompts;
use crate::records::{ConversationRecord, Entry, Mode, PlanState};
use crate::runtime::{AgentRuntime, NewConversation, RunEnd, Steer, random_id};
use crate::tool_round::{CallResult, CallScope};
use crate::traits::{AdmitOutcome, CommandAdmission, IntentChange, PlanAdmission, ToolOutcome};

/// `/plan [prompt]`: enter plan mode (then submit the prompt), or leave it
/// without approving anything.
pub(crate) async fn toggle(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    prompt: &str,
) -> Result<(), AgentError> {
    match record.mode {
        Mode::Normal => {
            enter(runtime, record).await?;
            if !prompt.trim().is_empty() && parse_slash_command(prompt).is_none() {
                runtime
                    .steer(&record.id, Steer::User(prompt.to_owned()))
                    .await?;
            }
            Ok(())
        }
        Mode::Plan => exit(runtime, &record.id, None).await.map(|_| ()),
    }
}

async fn enter(runtime: &AgentRuntime, record: &ConversationRecord) -> Result<(), AgentError> {
    let lifecycle = runtime.lifecycle_lock(&record.id);
    let lifecycle_guard = lifecycle.lock().await;
    if runtime.current_goal(&record.id).await?.is_some_and(|goal| {
        matches!(
            goal.status,
            roost_protocol::wire::agent_chat::GoalStatus::Active
                | roost_protocol::wire::agent_chat::GoalStatus::Paused
                | roost_protocol::wire::agent_chat::GoalStatus::BudgetLimited
        )
    }) {
        return Err(AgentError::FailedPrecondition(
            "Plan mode cannot run while a goal is active or paused".into(),
        ));
    }
    let updated = runtime
        .update_record(&record.id, |row| {
            row.mode = Mode::Plan;
            row.pre_plan_model = row.model.take();
        })
        .await?;
    runtime
        .append(
            &record.id,
            Entry::ModeChange {
                mode: Mode::Plan,
                plan_title: None,
            },
        )
        .await?;
    drop(lifecycle_guard);
    runtime.emit_agent(&updated).await;
    tracing::info!(conversation_id = %record.id, "plan mode entered");
    Ok(())
}

/// Leaves plan mode, restoring the model it replaced.
async fn exit(
    runtime: &AgentRuntime,
    id: &str,
    plan_title: Option<String>,
) -> Result<ConversationRecord, AgentError> {
    let lifecycle = runtime.lifecycle_lock(id);
    let lifecycle_guard = lifecycle.lock().await;
    let updated = runtime
        .update_record(id, |row| {
            row.mode = Mode::Normal;
            row.model = row.pre_plan_model.take();
        })
        .await?;
    runtime
        .append(
            id,
            Entry::ModeChange {
                mode: Mode::Normal,
                plan_title,
            },
        )
        .await?;
    drop(lifecycle_guard);
    runtime.emit_agent(&updated).await;
    tracing::info!(conversation_id = id, "plan mode left");
    Ok(updated)
}

/// The `propose_plan` tool: records the proposal and ends the run.
pub(crate) async fn propose(scope: &CallScope<'_>, args: &str) -> CallResult {
    let parsed = serde_json::from_str::<serde_json::Value>(args).ok();
    let field = |name: &str| {
        parsed
            .as_ref()
            .and_then(|value| value.get(name))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let (Some(title), Some(content)) = (field("title"), field("plan")) else {
        return CallResult::failure("propose_plan requires non-empty `title` and `plan`.");
    };
    let entry = Entry::PlanProposal {
        item_id: random_id("p-"),
        title,
        content,
        state: PlanState::Proposed,
    };
    if let Err(error) = scope.runtime.append(&scope.record.id, entry).await {
        return CallResult::failure(error.to_string());
    }
    CallResult {
        outcome: ToolOutcome {
            is_error: false,
            content: "Plan submitted. The user will approve it or ask for changes; stop here."
                .into(),
            details_json: "{}".into(),
        },
        end: Some(RunEnd::Completed),
    }
}

impl AgentRuntime {
    /// Applies the user's decision on a proposed plan. `approve_new` returns
    /// the new conversation's id.
    pub async fn plan_decide(
        &self,
        id: &str,
        item_id: &str,
        decision: &str,
        feedback: &str,
    ) -> Result<Option<String>, AgentError> {
        tracing::info!(conversation_id = id, item_id, decision, "plan decision");
        match decision {
            "approve" => {
                let (title, content) = self
                    .resolve_proposed_plan(id, item_id, PlanState::Approved)
                    .await?;
                let approved_prompt = prompts::render(
                    prompts::PLAN_APPROVED,
                    &[("title", &title), ("plan", &content)],
                );
                exit(self, id, Some(title)).await?;
                self.steer(id, Steer::User(approved_prompt)).await?;
                Ok(None)
            }
            "approve_new" => {
                let (title, content) = self
                    .resolve_proposed_plan(id, item_id, PlanState::Approved)
                    .await?;
                let approved_prompt = prompts::render(
                    prompts::PLAN_APPROVED,
                    &[("title", &title), ("plan", &content)],
                );
                let record = exit(self, id, Some(title.clone())).await?;
                let created = self
                    .create_conversation(NewConversation {
                        worker_fp: record.worker_fp.clone(),
                        worker_label: record.worker_label.clone(),
                        worker_os: record.worker_os.clone(),
                        cwd: record.cwd.clone(),
                        title: Some(title),
                        model: record.model.clone(),
                        thinking_level: Some(record.thinking_level.clone()),
                    })
                    .await?;
                self.steer(&created.id, Steer::User(approved_prompt))
                    .await?;
                Ok(Some(created.id))
            }
            "refine" => {
                if feedback.trim().is_empty() {
                    return Err(AgentError::InvalidArgument("refine needs feedback".into()));
                }
                self.resolve_proposed_plan(id, item_id, PlanState::Refined)
                    .await?;
                self.steer(id, Steer::User(feedback.to_owned())).await?;
                Ok(None)
            }
            "reject" => {
                self.resolve_proposed_plan(id, item_id, PlanState::Rejected)
                    .await?;
                exit(self, id, None).await?;
                self.resume_inputs_after_plan_rejection(id).await?;
                Ok(None)
            }
            other => Err(AgentError::InvalidArgument(format!(
                "unknown plan decision {other}"
            ))),
        }
    }

    async fn resolve_proposed_plan(
        &self,
        id: &str,
        item_id: &str,
        state: PlanState,
    ) -> Result<(String, String), AgentError> {
        let (title, content) = self.proposed_plan(id, item_id).await?;
        let entry = Entry::PlanProposal {
            item_id: item_id.to_owned(),
            title: title.clone(),
            content: content.clone(),
            state,
        };
        let outcome = self
            .inner
            .store
            .admit_command(
                id,
                CommandAdmission {
                    request_id: random_id("plan-resolve-"),
                    kind: "plan-resolve".into(),
                    entries: vec![entry.clone()],
                    intent: IntentChange::None,
                    plan_admission: PlanAdmission::RequireProposed {
                        item_id: item_id.to_owned(),
                    },
                    fence: None,
                    expected_intent_seq: None,
                    completion_through: None,
                },
            )
            .await?;
        let seqs = match outcome {
            AdmitOutcome::Admitted { seqs, .. } => seqs,
            AdmitOutcome::PlanNotProposed | AdmitOutcome::Duplicate { .. } => {
                return Err(AgentError::FailedPrecondition(
                    "this plan was already decided".into(),
                ));
            }
            AdmitOutcome::PendingPlan
            | AdmitOutcome::Busy
            | AdmitOutcome::NotWithdrawable
            | AdmitOutcome::Stopped => {
                return Err(AgentError::Store(
                    "plan resolution returned an unexpected admission result".into(),
                ));
            }
        };
        let sequence = seqs
            .first()
            .copied()
            .ok_or_else(|| AgentError::Store("plan resolution returned no sequence".into()))?;
        self.emit_entry(id, sequence, &entry).await?;
        Ok((title, content))
    }

    async fn resume_inputs_after_plan_rejection(&self, id: &str) -> Result<(), AgentError> {
        let lifecycle = self.lifecycle_lock(id);
        let _guard = lifecycle.lock().await;
        let entries = self.inner.store.entries(id).await?;
        let Some((intent_sequence, through_seq)) =
            entries.iter().rev().find_map(|(sequence, entry)| {
                let Entry::RunIntent { through_seq, .. } = entry else {
                    return None;
                };
                Some((*sequence, *through_seq))
            })
        else {
            return Ok(());
        };
        let has_late_submission = crate::runtime::latest_submission_seq(&entries)
            .is_some_and(|sequence| sequence > through_seq);
        let has_queued_input = !crate::admission::queued_inputs_to_activate(&entries).is_empty();
        if !has_late_submission && !has_queued_input {
            return Ok(());
        }
        let Some(fence) = self.claim_lease(id).await? else {
            return Ok(());
        };
        let outcome = self
            .inner
            .store
            .admit_command(
                id,
                CommandAdmission {
                    request_id: random_id("plan-reject-resume-"),
                    kind: "plan-reject-resume".into(),
                    entries: Vec::new(),
                    intent: IntentChange::Complete,
                    plan_admission: PlanAdmission::AllowPending,
                    fence: Some(fence),
                    expected_intent_seq: Some(intent_sequence),
                    completion_through: Some(through_seq),
                },
            )
            .await?;
        let seqs = match outcome {
            AdmitOutcome::Admitted { seqs, .. } => seqs,
            AdmitOutcome::Stopped
            | AdmitOutcome::Duplicate { .. }
            | AdmitOutcome::Busy
            | AdmitOutcome::PendingPlan
            | AdmitOutcome::PlanNotProposed
            | AdmitOutcome::NotWithdrawable => {
                self.release_lease(id).await;
                return Ok(());
            }
        };
        let updated = self.inner.store.entries(id).await?;
        for sequence in &seqs {
            if let Some((_, entry)) = updated.iter().find(|(found, _)| found == sequence) {
                self.emit_entry(id, *sequence, entry).await?;
            }
        }
        let Some(intent_sequence) = seqs.last().copied() else {
            self.release_lease(id).await;
            return Err(AgentError::Store(
                "plan rejection resume appended no run intent".into(),
            ));
        };
        match self
            .start_run_now(id, None, None, Some(intent_sequence))
            .await
        {
            Ok(Some(_)) | Ok(None) => Ok(()),
            Err(error) => {
                self.release_lease(id).await;
                self.schedule_readmission(id);
                Err(error)
            }
        }
    }

    async fn proposed_plan(&self, id: &str, item_id: &str) -> Result<(String, String), AgentError> {
        let entries = self.inner.store.entries(id).await?;
        let mut rewound_from = u64::MAX;
        for (sequence, entry) in entries.iter().rev() {
            match entry {
                Entry::Rewind { to_seq } => rewound_from = rewound_from.min(*to_seq),
                Entry::PlanProposal {
                    item_id: found,
                    title,
                    content,
                    state,
                } if *sequence < rewound_from && found == item_id => {
                    return match state {
                        PlanState::Proposed => Ok((title.clone(), content.clone())),
                        _ => Err(AgentError::FailedPrecondition(
                            "this plan was already decided".into(),
                        )),
                    };
                }
                _ => {}
            }
        }
        Err(AgentError::NotFound(format!("plan {item_id}")))
    }
}
