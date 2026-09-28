//! The checks an agent prompt must pass before any keeper byte: the request's
//! own shape, its downstream-hop budget, and the status fence the coordinator
//! quoted against the worker-private proof. Pure functions over the request and
//! the proof, so every rejection they return provably wrote nothing. Called by
//! `super::prompt_control`. Ports `validateRequest`, `checkBudget`,
//! `processProofMatches` and `statusFenceFailure` of
//! `apps/worker/src/agents/agent-prompt-control.ts`.

use std::time::Duration;

use roost_proto::DAgentPrompt;
use roost_protocol::terminal_input::is_valid_agent_prompt_text;
use roost_protocol::wire::agent_status::{
    AgentOccupantId, AgentRuntimeState, AgentStatusSource, StatusEpoch,
};
use roost_protocol::wire::brand::SessionId;

use super::process_scan::AgentProcessIdentity;
use super::registry::AgentStatusPrivateProof;
use crate::uplink::TERMINAL_REQUEST_BUDGET_CAP_MS;

/// v2 counts the request id in UTF-16 code units, and so does this.
const REQUEST_ID_MAX_LENGTH: usize = 128;
/// The coordinator's revision is a JavaScript number on the other side, so
/// only the exactly representable range is a revision it can have quoted.
const MAX_SAFE_REVISION: u64 = (1 << 53) - 1;

/// v2 `TerminalRequestBudget` as prompt admission reads it: which connection
/// the request arrived on, and how long its budget has left.
pub trait PromptBudget: Send + Sync {
    /// The connection the request arrived on is still the current one.
    fn is_current_connection(&self) -> bool;
    /// What remains of the request budget, zero once it is spent.
    fn remaining(&self) -> Duration;
}

/// The request fields every later fence compares against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPrompt {
    pub session_id: SessionId,
    pub expected_revision: i64,
}

/// v2 `validateRequest`: the shape a prompt must have before anything is read.
pub fn validate_prompt_request(request: &DAgentPrompt) -> Result<ValidatedPrompt, &'static str> {
    let request_id_length = request.request_id.encode_utf16().count();
    if request_id_length == 0 || request_id_length > REQUEST_ID_MAX_LENGTH {
        return Err("request_id is invalid");
    }
    let Ok(session_id) = SessionId::try_from(request.session_id.as_str()) else {
        return Err("session_id must be a UUID");
    };
    if request.input_seq == 0 {
        return Err("input sequence must be a positive uint64");
    }
    if StatusEpoch::try_from(request.expected_status_epoch.as_str()).is_err() {
        return Err("expected_status_epoch must be a UUID");
    }
    if AgentOccupantId::try_from(request.expected_occupant_id.as_str()).is_err() {
        return Err("expected_occupant_id must be a UUID");
    }
    let expected_revision = match i64::try_from(request.expected_revision) {
        Ok(revision) if request.expected_revision <= MAX_SAFE_REVISION => revision,
        _ => return Err("expected_revision must be a safe uint64"),
    };
    if !is_valid_agent_prompt_text(&request.text) {
        return Err("prompt text is invalid");
    }
    if request.budget_ms < 1 || request.budget_ms > TERMINAL_REQUEST_BUDGET_CAP_MS {
        return Err("budget_ms is invalid");
    }
    Ok(ValidatedPrompt {
        session_id,
        expected_revision,
    })
}

/// v2 `checkBudget`: the time left, or why the request may no longer write.
pub fn check_prompt_budget(budget: &dyn PromptBudget) -> Result<Duration, &'static str> {
    if !budget.is_current_connection() {
        return Err("worker connection was superseded");
    }
    let remaining = budget.remaining();
    if remaining.is_zero() {
        return Err("prompt budget expired");
    }
    Ok(remaining)
}

/// v2 `processProofMatches`: the same agent, as the same process.
pub fn process_proof_matches(proof: &AgentProcessIdentity, expected: &AgentProcessIdentity) -> bool {
    proof.agent_id == expected.agent_id && proof.pid == expected.pid
}

/// v2 `statusFenceFailure`: the proof the prompt may write under, or why not.
/// Only an integration-reported agent that is idle or working admits a prompt.
pub fn status_fence<'proof>(
    proof: Option<&'proof AgentStatusPrivateProof>,
    request: &DAgentPrompt,
    expected_revision: i64,
) -> Result<&'proof AgentStatusPrivateProof, &'static str> {
    let Some(proof) = proof else {
        return Err("agent status is unavailable");
    };
    if proof.status_epoch.as_str() != request.expected_status_epoch
        || proof.occupant_id.as_str() != request.expected_occupant_id
        || proof.revision != expected_revision
    {
        return Err("agent status fence changed");
    }
    if proof.source != AgentStatusSource::Integration {
        return Err("agent status source is not integration");
    }
    match proof.state {
        AgentRuntimeState::Blocked => Err("agent is blocked"),
        AgentRuntimeState::Idle | AgentRuntimeState::Working => Ok(proof),
    }
}
