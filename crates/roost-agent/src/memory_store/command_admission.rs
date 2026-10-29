//! Atomic command and run-intent transitions under the memory store's lock.
//! Mirrors the SQL store's transactions through the shared admission predicates.
//! Conditional restarts cannot overwrite a later Stop or a newer submission.

use crate::admission::{
    activate_followup, has_pending_plan, intent_entry, plan_is_proposed, queued_inputs_to_activate,
    restart_boundary_matches, submission_is_withdrawable,
};
use crate::error::AgentError;
use crate::records::Entry;
use crate::traits::{AdmitOutcome, CommandAdmission, IntentChange, PlanAdmission};

use super::MemoryState;

impl MemoryState {
    pub(super) fn admit_command(
        &mut self,
        id: &str,
        admission: CommandAdmission,
    ) -> Result<AdmitOutcome, AgentError> {
        if !self.conversations.contains_key(id) {
            return Err(AgentError::NotFound(id.to_owned()));
        }
        let key = (id.to_owned(), admission.request_id.clone());
        if let Some(result_json) = self.commands.get(&key) {
            return Ok(AdmitOutcome::Duplicate {
                result_json: result_json.clone(),
            });
        }
        let journal = self.entries.get(id).cloned().unwrap_or_default();
        let pending_plan = has_pending_plan(&journal);
        match &admission.plan_admission {
            PlanAdmission::AllowPending => {}
            PlanAdmission::RejectIfPending if pending_plan => {
                return Ok(AdmitOutcome::PendingPlan);
            }
            PlanAdmission::RejectIfPending => {}
            PlanAdmission::RequireProposed { item_id } if !plan_is_proposed(&journal, item_id) => {
                return Ok(AdmitOutcome::PlanNotProposed);
            }
            PlanAdmission::RequireProposed { .. } => {}
        }
        if matches!(
            admission.intent,
            IntentChange::Complete | IntentChange::RestartIfCurrent
        ) {
            let Some(fence) = admission.fence.as_ref() else {
                return Err(AgentError::Fenced(id.to_owned()));
            };
            if !self.holds(id, fence) {
                return Err(AgentError::Fenced(id.to_owned()));
            }
        }
        if admission.intent == IntentChange::RestartIfCurrent
            && !restart_boundary_matches(&journal, admission.expected_intent_seq)
        {
            return Ok(AdmitOutcome::Stopped);
        }
        if admission.intent == IntentChange::Complete {
            let expected_intent = admission.expected_intent_seq.unwrap_or(0);
            if journal.iter().any(|(sequence, entry)| {
                *sequence > expected_intent
                    && matches!(entry, Entry::RunIntent { aborted: true, .. })
            }) {
                return Ok(AdmitOutcome::Stopped);
            }
        }
        if admission.intent == IntentChange::Withdraw {
            let submission_id = admission.entries.iter().find_map(|entry| match entry {
                Entry::QueuedInputWithdrawn { submission_id } => Some(submission_id),
                _ => None,
            });
            if submission_id
                .is_none_or(|submission_id| !submission_is_withdrawable(&journal, submission_id))
            {
                return Ok(AdmitOutcome::NotWithdrawable);
            }
        }
        let busy = journal
            .iter()
            .rev()
            .find_map(|(_, entry)| match entry {
                Entry::RunIntent { active, .. } => Some(*active),
                _ => None,
            })
            .unwrap_or(false);
        if admission.intent == IntentChange::RejectIfBusy && busy {
            return Ok(AdmitOutcome::Busy);
        }
        let queued = admission.intent == IntentChange::FollowUp && busy;
        let intent = match admission.intent {
            IntentChange::RejectIfBusy => IntentChange::Activate,
            IntentChange::FollowUp if busy => IntentChange::None,
            IntentChange::FollowUp => IntentChange::Activate,
            intent => intent,
        };
        let mut entries = admission.entries;
        if admission.intent == IntentChange::FollowUp && !busy {
            entries = entries
                .into_iter()
                .flat_map(|entry| activate_followup(entry).into_iter().flatten())
                .collect();
        }
        if intent == IntentChange::Complete && !pending_plan {
            entries.extend(queued_inputs_to_activate(&journal));
        }
        let last_before = self.last_seq(id);
        let mut seqs: Vec<u64> = entries
            .into_iter()
            .map(|entry| self.append(id, entry))
            .collect();
        if admission.intent == IntentChange::Complete {
            let through_seq = admission.completion_through.unwrap_or(last_before);
            let has_unconsumed = !pending_plan
                && self.entries.get(id).is_some_and(|journal| {
                    crate::journal::effective_entries(journal)
                        .iter()
                        .any(|(sequence, entry)| {
                            *sequence > through_seq && matches!(entry, Entry::User { .. })
                        })
                });
            seqs.push(self.append(
                id,
                Entry::RunIntent {
                    active: has_unconsumed,
                    through_seq,
                    aborted: false,
                    yielded: false,
                },
            ));
        } else if let Some(intent) = intent_entry(intent, last_before, seqs.last().copied()) {
            seqs.push(self.append(id, intent));
        }
        self.commands.insert(key, None);
        Ok(AdmitOutcome::Admitted { seqs, queued })
    }
}
