//! Status-fenced agent prompt admission, from a coordinator `agentPrompt` to
//! the acknowledged keeper writes that carry it. Depends on the worker-private
//! status proof (`super::registry`), the pane's foreground job
//! (`super::process_tree`), and the same keeper-input lane and text encoder as
//! interactive input (`session::input_write`, `roost_protocol::terminal_input`).
//! Called by `super::prompt_port`. Ports `apps/worker/src/agents/agent-prompt-control.ts`.
//!
//! Every return before the first keeper write is a proven zero-write
//! rejection; from the first write on, the keeper's answers are the truth.

use std::sync::{Arc, Mutex};

use roost_proto::DAgentPrompt;
use roost_protocol::terminal_input::build_pty_payload;
use roost_protocol::wire::brand::SessionId;

use super::process_scan::AgentProcessIdentity;
use super::process_snapshot::ScanAbort;
use super::process_tree::agent_owns_terminal_foreground;
use super::prompt_fence::{
    PromptBudget, check_prompt_budget, process_proof_matches, status_fence, validate_prompt_request,
};
use super::prompt_submit::{PROMPT_SUBMIT_DELAY, submit_agent_prompt};
use super::registry::AgentStatusPrivateProof;
use crate::session::input_write::{HeldInputLane, WorkerInputResult};
use crate::session::lifecycle::SessionManager;
use crate::session::table::SessionTable;
use crate::session::types::SessionRecord;
use crate::uplink::OwnerFuture;

/// The rejection for a pane whose foreground job is not the agent's.
pub const NOT_FOREGROUND_REASON: &str = "agent is not the terminal foreground process";

/// The status fence source. v2 `Pick<AgentStatusRegistry, "currentPrivateProof">`.
pub trait AgentStatusProofs: Send + Sync {
    fn current_private_proof(&self, session_id: &SessionId) -> Option<AgentStatusPrivateProof>;
}

/// The live process proof. v2 `Pick<AgentScreenDetector, "reportingAgentForSession">`;
/// `abort` is v2's `AbortSignal`, raised when the prompt budget runs out.
pub trait AgentProcessProver: Send + Sync {
    fn reporting_agent_for_session(
        &self,
        session_id: &SessionId,
        reporter_pid: u32,
        abort: Option<ScanAbort>,
    ) -> OwnerFuture<Option<AgentProcessIdentity>>;
}

/// What prompt admission reads. v2 `AgentPromptControlDeps`.
pub struct AgentPromptControlDeps {
    pub manager: Arc<SessionManager>,
    pub sessions: Arc<SessionTable>,
    pub registry: Arc<dyn AgentStatusProofs>,
    pub detector: Arc<dyn AgentProcessProver>,
}

impl std::fmt::Debug for AgentPromptControlDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentPromptControlDeps")
            .finish_non_exhaustive()
    }
}

/// The exact record a prompt was admitted against, compared by identity: a
/// respawn under the same session id is a different record. v2 `exactSession`.
struct ExpectedSession {
    channel_id: u16,
    record: Arc<Mutex<SessionRecord>>,
}

impl ExpectedSession {
    fn capture(sessions: &SessionTable, session_id: &SessionId) -> Option<Self> {
        let channel_id = sessions.channel_of(session_id)?;
        let record = sessions.record_of_channel(channel_id)?;
        Some(Self { channel_id, record })
    }

    fn still_exact(&self, sessions: &SessionTable, session_id: &SessionId) -> bool {
        sessions.channel_of(session_id) == Some(self.channel_id)
            && sessions
                .record_of_channel(self.channel_id)
                .is_some_and(|current| Arc::ptr_eq(&current, &self.record))
    }
}

/// Admit exactly one prompt and write it, or say why nothing was written.
pub async fn write_agent_prompt(
    request: &DAgentPrompt,
    budget: &dyn PromptBudget,
    deps: &AgentPromptControlDeps,
) -> WorkerInputResult {
    let outcome = admit_agent_prompt(request, budget, deps).await;
    // The request id and the static reason only: the prompt text and the
    // status message never leave the worker, not even into its log.
    match &outcome {
        WorkerInputResult::Accepted { written_bytes } => tracing::info!(
            request_id = %request.request_id, session = %request.session_id, written_bytes,
            "an agent prompt was written and submitted"
        ),
        WorkerInputResult::Rejected { reason } => tracing::info!(
            request_id = %request.request_id, session = %request.session_id, reason = %reason,
            "an agent prompt was refused before any keeper write"
        ),
        WorkerInputResult::Ambiguous {
            written_bytes,
            reason,
        } => tracing::warn!(
            request_id = %request.request_id, session = %request.session_id, written_bytes, reason = %reason,
            "an agent prompt reached the keeper with an unconfirmed outcome"
        ),
    }
    outcome
}

async fn admit_agent_prompt(
    request: &DAgentPrompt,
    budget: &dyn PromptBudget,
    deps: &AgentPromptControlDeps,
) -> WorkerInputResult {
    let validated = match validate_prompt_request(request) {
        Ok(validated) => validated,
        Err(reason) => return rejected(reason),
    };
    if let Err(reason) = check_prompt_budget(budget) {
        return rejected(reason);
    }
    let session_id = &validated.session_id;
    let Some(expected) = ExpectedSession::capture(&deps.sessions, session_id) else {
        return rejected("session is not live");
    };
    let initial_status = deps.registry.current_private_proof(session_id);
    let initial = match status_fence(
        initial_status.as_ref(),
        request,
        validated.expected_revision,
    ) {
        Ok(proof) => proof.clone(),
        Err(reason) => return rejected(reason),
    };
    // The lane is taken NOW, before the first scan, so the prompt keeps the
    // receive order it arrived in against any input that follows it.
    let lane = match deps.manager.admit_held_input(expected.channel_id) {
        Ok(lane) => lane,
        Err(reason) => return rejected(reason),
    };
    let fence = PromptFence {
        request,
        session_id,
        expected_revision: validated.expected_revision,
        budget,
        deps,
        expected: &expected,
    };
    let outcome = fence.write_on(&lane, &initial).await;
    lane.release();
    outcome
}

/// One admitted prompt's fixed inputs, carried through the queued half.
struct PromptFence<'a> {
    request: &'a DAgentPrompt,
    session_id: &'a SessionId,
    expected_revision: i64,
    budget: &'a dyn PromptBudget,
    deps: &'a AgentPromptControlDeps,
    expected: &'a ExpectedSession,
}

impl PromptFence<'_> {
    async fn write_on(
        &self,
        lane: &HeldInputLane,
        initial: &AgentStatusPrivateProof,
    ) -> WorkerInputResult {
        let mut grant = std::pin::pin!(lane.granted());
        let mut granted = false;
        // The grant is polled beside the first scan so the ticket holds its
        // place in the lane while the scan runs, exactly as a v2 ticket does
        // from the moment it is issued.
        let first_refresh = {
            let mut refresh = std::pin::pin!(self.refresh_process_proof(&initial.process));
            loop {
                tokio::select! {
                    biased;
                    () = &mut grant, if !granted => granted = true,
                    outcome = &mut refresh => break outcome,
                }
            }
        };
        let first_proof = match first_refresh {
            Ok(Some(proof)) => proof,
            Ok(None) => return rejected("agent process proof could not be refreshed"),
            Err(reason) => return rejected(reason),
        };
        if !agent_owns_terminal_foreground(first_proof.foreground.as_ref()) {
            return rejected(NOT_FOREGROUND_REASON);
        }
        if !self
            .expected
            .still_exact(&self.deps.sessions, self.session_id)
        {
            return rejected("session changed before prompt admission");
        }
        let pre_admission = match self.current_fence() {
            Ok(proof) => proof,
            Err(reason) => return rejected(reason),
        };
        if !process_proof_matches(&pre_admission.process, &first_proof) {
            return rejected("agent process proof changed before prompt admission");
        }
        if let Err(reason) = check_prompt_budget(self.budget) {
            return rejected(reason);
        }
        if !granted && let Err(reason) = self.wait_for_grant(grant.as_mut()).await {
            return rejected(reason);
        }
        if let Err(reason) = check_prompt_budget(self.budget) {
            return rejected(reason);
        }
        tracing::debug!(request_id = %self.request.request_id, "an agent prompt holds the keeper input lane");
        self.write_granted(lane, &first_proof).await
    }

    async fn write_granted(
        &self,
        lane: &HeldInputLane,
        first_proof: &AgentProcessIdentity,
    ) -> WorkerInputResult {
        let final_refresh = match self.refresh_process_proof(first_proof).await {
            Ok(proof) => proof,
            Err(reason) => return rejected(reason),
        };
        let payload = match self.expected.record.lock() {
            Ok(record) => {
                build_pty_payload(&self.request.text, record.terminal_core.bracketed_paste())
            }
            Err(_) => return rejected("terminal input mode could not be read"),
        };
        if !self
            .expected
            .still_exact(&self.deps.sessions, self.session_id)
        {
            return rejected("session changed before the keeper write");
        }
        let final_status = match self.current_fence() {
            Ok(proof) => proof,
            Err(reason) => return rejected(reason),
        };
        let Some(final_proof) = final_refresh.filter(|proof| {
            process_proof_matches(proof, first_proof)
                && process_proof_matches(&final_status.process, proof)
        }) else {
            return rejected("agent process proof changed before the keeper write");
        };
        if !agent_owns_terminal_foreground(final_proof.foreground.as_ref()) {
            return rejected(NOT_FOREGROUND_REASON);
        }
        let remaining = match check_prompt_budget(self.budget) {
            Ok(remaining) => remaining,
            Err(reason) => return rejected(reason),
        };
        // The CR is a second write PROMPT_SUBMIT_DELAY after the text, so a
        // budget that cannot cover it would strand the text as an unsubmitted
        // draft with no way to finish the submission.
        if remaining <= PROMPT_SUBMIT_DELAY {
            return rejected("prompt budget cannot cover the submit delay");
        }
        lane.mark_input_sensitive();
        submit_agent_prompt(lane, payload, || check_prompt_budget(self.budget).is_ok()).await
    }

    /// The status proof as it stands now, if it still admits this prompt.
    fn current_fence(&self) -> Result<AgentStatusPrivateProof, &'static str> {
        let current = self.deps.registry.current_private_proof(self.session_id);
        status_fence(current.as_ref(), self.request, self.expected_revision).cloned()
    }

    /// v2 `refreshProcessProof`: a fresh scan bounded by the request budget. A
    /// scan that outlives the budget is aborted and the budget failure wins.
    async fn refresh_process_proof(
        &self,
        expected: &AgentProcessIdentity,
    ) -> Result<Option<AgentProcessIdentity>, &'static str> {
        let remaining = check_prompt_budget(self.budget)?;
        let abort = ScanAbort::new();
        let scan = self.deps.detector.reporting_agent_for_session(
            self.session_id,
            expected.pid,
            Some(abort.clone()),
        );
        let Ok(refreshed) = tokio::time::timeout(remaining, scan).await else {
            abort.abort();
            tracing::info!(request_id = %self.request.request_id, "an agent process scan outlived the prompt budget and was aborted");
            return Err("prompt budget expired");
        };
        check_prompt_budget(self.budget)?;
        Ok(refreshed.filter(|proof| process_proof_matches(proof, expected)))
    }

    /// v2 `waitForAdmissionGrant`: the lane, within the request budget.
    async fn wait_for_grant(
        &self,
        grant: std::pin::Pin<&mut impl Future<Output = ()>>,
    ) -> Result<(), &'static str> {
        let remaining = check_prompt_budget(self.budget)?;
        if tokio::time::timeout(remaining, grant).await.is_err() {
            return Err("prompt budget expired");
        }
        Ok(())
    }
}

fn rejected(reason: &str) -> WorkerInputResult {
    WorkerInputResult::Rejected {
        reason: reason.to_owned(),
    }
}
