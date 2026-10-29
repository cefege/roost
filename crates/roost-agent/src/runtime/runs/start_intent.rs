//! Persists the start boundary for explicit runs and conditional automatic restarts.
//! Automatic starts compare the settlement boundary inside the store transaction,
//! so a Stop or newer admission cannot be overwritten while the lifecycle lock changes hands.

use crate::error::AgentError;
use crate::runtime::{AgentRuntime, random_id};
use crate::traits::{AdmitOutcome, CommandAdmission, IntentChange, PlanAdmission};

impl AgentRuntime {
    pub(super) async fn commit_start_intent(
        &self,
        id: &str,
        expected_intent: Option<u64>,
    ) -> Result<Option<u64>, AgentError> {
        let admission = match expected_intent {
            Some(expected_intent) => CommandAdmission {
                request_id: random_id("run-restart-"),
                kind: "run-restart".to_owned(),
                entries: Vec::new(),
                intent: IntentChange::RestartIfCurrent,
                plan_admission: PlanAdmission::RejectIfPending,
                fence: self.held_lease(id),
                expected_intent_seq: Some(expected_intent),
                completion_through: None,
            },
            None => CommandAdmission {
                request_id: random_id("run-start-"),
                kind: "run-start".to_owned(),
                entries: Vec::new(),
                intent: IntentChange::Activate,
                plan_admission: PlanAdmission::RejectIfPending,
                fence: None,
                expected_intent_seq: None,
                completion_through: None,
            },
        };
        let outcome = self.inner.store.admit_command(id, admission).await?;
        match outcome {
            AdmitOutcome::Admitted { seqs, .. } => {
                seqs.last().copied().map(Some).ok_or_else(|| {
                    AgentError::Store("run start did not append its run intent".to_owned())
                })
            }
            AdmitOutcome::Stopped | AdmitOutcome::Duplicate { .. } | AdmitOutcome::PendingPlan => {
                Ok(None)
            }
            AdmitOutcome::Busy | AdmitOutcome::PlanNotProposed | AdmitOutcome::NotWithdrawable => {
                Err(AgentError::Store(
                    "run start admission returned a submission-only result".to_owned(),
                ))
            }
        }
    }
}
