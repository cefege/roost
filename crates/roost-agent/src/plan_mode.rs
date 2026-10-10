//! Ported from oh-my-pi packages/coding-agent/src/plan-mode/ (MIT).
//! Plan mode: `/plan` toggles a read-only planning mode on the `plan` role's
//! model; `propose_plan` records a proposal and ends the run; the user's
//! decision approves (here or in a new chat) or asks for refinement.

use roost_protocol::wire::agent_chat::parse_slash_command;

use crate::error::AgentError;
use crate::prompts;
use crate::records::{ConversationRecord, Entry, Mode, PlanState, Role, model_ref};
use crate::roles;
use crate::runtime::{AgentRuntime, NewConversation, RunEnd, Steer, random_id};
use crate::tool_round::{CallResult, CallScope};
use crate::traits::ToolOutcome;

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
    let settings = runtime.settings().await?;
    let plan_model = if settings.model_roles.contains_key(&Role::Plan) {
        roles::resolve_role(runtime.inner.llm.as_ref(), &settings, Role::Plan).await
    } else {
        None
    };
    let updated = runtime
        .update_record(&record.id, |row| {
            row.mode = Mode::Plan;
            row.pre_plan_model = row.model.clone();
            if let Some(model) = &plan_model {
                row.model = Some(model_ref(&model.info.provider, &model.info.id));
            }
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
    let updated = runtime
        .update_record(id, |row| {
            row.mode = Mode::Normal;
            if let Some(model) = row.pre_plan_model.take() {
                row.model = Some(model);
            }
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
        let (title, content) = self.proposed_plan(id, item_id).await?;
        let mark = |state| Entry::PlanProposal {
            item_id: item_id.to_owned(),
            title: title.clone(),
            content: content.clone(),
            state,
        };
        let approved_prompt = prompts::render(
            prompts::PLAN_APPROVED,
            &[("title", &title), ("plan", &content)],
        );
        tracing::info!(conversation_id = id, item_id, decision, "plan decision");
        match decision {
            "approve" => {
                self.append(id, mark(PlanState::Approved)).await?;
                exit(self, id, Some(title.clone())).await?;
                self.steer(id, Steer::User(approved_prompt)).await?;
                Ok(None)
            }
            "approve_new" => {
                self.append(id, mark(PlanState::Approved)).await?;
                let record = exit(self, id, Some(title.clone())).await?;
                let created = self
                    .create_conversation(NewConversation {
                        worker_fp: record.worker_fp.clone(),
                        worker_label: record.worker_label.clone(),
                        worker_os: record.worker_os.clone(),
                        cwd: record.cwd.clone(),
                        title: Some(title.clone()),
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
                self.append(id, mark(PlanState::Refined)).await?;
                self.steer(id, Steer::User(feedback.to_owned())).await?;
                Ok(None)
            }
            other => Err(AgentError::InvalidArgument(format!(
                "unknown plan decision {other}"
            ))),
        }
    }

    async fn proposed_plan(&self, id: &str, item_id: &str) -> Result<(String, String), AgentError> {
        let entries = self.inner.store.entries(id).await?;
        let latest = entries.iter().rev().find_map(|(_, entry)| match entry {
            Entry::PlanProposal {
                item_id: found,
                title,
                content,
                state,
            } if found == item_id => Some((title.clone(), content.clone(), *state)),
            _ => None,
        });
        match latest {
            Some((title, content, PlanState::Proposed)) => Ok((title, content)),
            Some(_) => Err(AgentError::FailedPrecondition(
                "this plan was already decided".into(),
            )),
            None => Err(AgentError::NotFound(format!("plan {item_id}"))),
        }
    }
}
