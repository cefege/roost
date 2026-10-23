//! Persists the start boundary for explicit runs and conditional automatic restarts.
//! Automatic starts compare the settlement boundary inside the store transaction,
//! so a Stop or newer admission cannot be overwritten while the lifecycle lock changes hands.

use crate::error::AgentError;
use crate::records::Entry;
use crate::runtime::{AgentRuntime, random_id};
use crate::traits::{AdmitOutcome, CommandAdmission, IntentChange};

impl AgentRuntime {
    pub(super) async fn commit_start_intent(
        &self,
        id: &str,
        through_seq: u64,
        expected_intent: Option<u64>,
    ) -> Result<Option<u64>, AgentError> {
        let Some(expected_intent) = expected_intent else {
            return self
                .append(
                    id,
                    Entry::RunIntent {
                        active: true,
                        through_seq,
                        aborted: false,
                        yielded: false,
                    },
                )
                .await
                .map(Some);
        };
        let outcome = self
            .inner
            .store
            .admit_command(
                id,
                CommandAdmission {
                    request_id: random_id("run-restart-"),
                    kind: "run-restart".to_owned(),
                    entries: Vec::new(),
                    intent: IntentChange::RestartIfCurrent,
                    fence: self.held_lease(id),
                    expected_intent_seq: Some(expected_intent),
                    completion_through: None,
                },
            )
            .await?;
        match outcome {
            AdmitOutcome::Admitted { seqs, .. } => {
                seqs.last().copied().map(Some).ok_or_else(|| {
                    AgentError::Store("restart did not append its run intent".to_owned())
                })
            }
            AdmitOutcome::Stopped | AdmitOutcome::Duplicate { .. } => Ok(None),
            AdmitOutcome::Busy | AdmitOutcome::NotWithdrawable => Err(AgentError::Store(
                "restart admission returned a submission-only result".to_owned(),
            )),
        }
    }
}
