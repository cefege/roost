//! At-most-once client commands that are not plain input: answers, plan
//! decisions, aborts, rewinds, forks and slash commands. The request row is
//! committed first; a retry of the same request returns the recorded outcome
//! instead of applying the command again.

use serde::{Deserialize, Serialize};

use super::AgentRuntime;
use crate::error::AgentError;
use crate::traits::{AdmitOutcome, CommandAdmission, IntentChange, PlanAdmission};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Ok(String),
    Error(String),
}

impl AgentRuntime {
    /// Runs `apply` once per `(conversation, request_id)`. A retry returns the
    /// first attempt's result, or its error as `FailedPrecondition`; a retry
    /// while the first is still applying returns `None`.
    pub async fn deduplicated<F, Fut>(
        &self,
        id: &str,
        request_id: &str,
        kind: &str,
        apply: F,
    ) -> Result<Option<String>, AgentError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String, AgentError>>,
    {
        self.deduplicated_with_plan_admission(
            id,
            request_id,
            kind,
            PlanAdmission::AllowPending,
            apply,
        )
        .await
    }

    /// Runs `apply` once, refusing a new command while a plan awaits a decision.
    pub async fn deduplicated_requiring_resolved_plan<F, Fut>(
        &self,
        id: &str,
        request_id: &str,
        kind: &str,
        apply: F,
    ) -> Result<Option<String>, AgentError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String, AgentError>>,
    {
        self.deduplicated_with_plan_admission(
            id,
            request_id,
            kind,
            PlanAdmission::RejectIfPending,
            apply,
        )
        .await
    }

    pub(crate) async fn deduplicated_with_plan_admission<F, Fut>(
        &self,
        id: &str,
        request_id: &str,
        kind: &str,
        plan_admission: PlanAdmission,
        apply: F,
    ) -> Result<Option<String>, AgentError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String, AgentError>>,
    {
        if request_id.trim().is_empty() {
            return Err(AgentError::InvalidArgument("request_id is required".into()));
        }
        let admitted = self
            .inner
            .store
            .admit_command(
                id,
                CommandAdmission {
                    request_id: request_id.to_owned(),
                    kind: kind.to_owned(),
                    entries: Vec::new(),
                    intent: IntentChange::None,
                    fence: None,
                    expected_intent_seq: None,
                    completion_through: None,
                    plan_admission,
                },
            )
            .await?;
        match &admitted {
            AdmitOutcome::PendingPlan => {
                return Err(AgentError::FailedPrecondition(
                    "approve, refine, or reject the proposed plan before sending another message"
                        .into(),
                ));
            }
            AdmitOutcome::PlanNotProposed => {
                return Err(AgentError::FailedPrecondition(
                    "this plan was already decided".into(),
                ));
            }
            _ => {}
        }
        if let AdmitOutcome::Duplicate { result_json } = admitted {
            tracing::info!(
                conversation_id = id,
                request_id,
                kind,
                "duplicate agent command answered from its record"
            );
            return match result_json.map(|json| serde_json::from_str::<Outcome>(&json)) {
                None => Ok(None),
                Some(Ok(Outcome::Ok(result))) => Ok(Some(result)),
                Some(Ok(Outcome::Error(message))) => Err(AgentError::FailedPrecondition(message)),
                Some(Err(error)) => Err(AgentError::Store(error.to_string())),
            };
        }
        let result = apply().await;
        let outcome = match &result {
            Ok(value) => Outcome::Ok(value.clone()),
            Err(error) => Outcome::Error(error.to_string()),
        };
        let json = serde_json::to_string(&outcome)
            .map_err(|error| AgentError::Store(error.to_string()))?;
        if let Err(error) = self
            .inner
            .store
            .complete_command(id, request_id, &json)
            .await
        {
            tracing::warn!(conversation_id = id, request_id, %error, "agent command outcome was not recorded");
        }
        result.map(Some)
    }
}
