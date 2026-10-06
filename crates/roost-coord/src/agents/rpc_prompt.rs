//! `SessionsPrompt`: authorize an open session, validate every public fence and
//! bound, then delegate exactly-once write/wait orchestration to
//! `agents::prompt_control`. Responses expose only bounded outcomes; prompt
//! text, worker reasons and agent status messages never enter logs or replies.
//! Called from the arm in `rpc/service_impl.rs`.
//! Ports `apps/coord/src/agents/handlers-agent-prompt.ts`.

use std::collections::HashSet;

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_observability::LogFields;
use roost_proto::buffa::EnumValue;
use roost_proto::{
    AgentPromptInputOutcome, AgentPromptRejection, AgentPromptWaitOutcome as WireWaitOutcome,
    SessionsPromptRequest, SessionsPromptResponse,
};
use roost_protocol::terminal_input::{
    AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS, AGENT_PROMPT_WAIT_TIMEOUT_MIN_MS, is_valid_agent_prompt_text,
};
use roost_protocol::wire::{AgentOccupantId, SessionId, StatusEpoch};

use crate::agents::prompt_control::{
    AgentPromptControlCommand, AgentPromptWaitConfig, AgentPromptWaitOutcome,
    process_agent_prompt_control,
};
use crate::agents::status_wait::{
    AgentStatusWaitError, AgentStatusWaitErrorKind, AgentStatusWaitOutcome,
};
use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;
use crate::terminal_input::control_lane::terminal_viewer_identity;
use crate::terminal_input::write_control::{
    TerminalWriteControlResult, TerminalWriteStatus, terminal_write_rejected,
};

/// The largest revision a JavaScript peer can have sent without precision loss.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Every definite rejection the worker or this coordinator can produce, mapped
/// to the one member a caller may branch on: `blocked` means stop and send keys
/// interactively, a changed fence or process means re-read status and retry,
/// and `expired` means the same request is still valid. The free-text reason
/// stays private, so an unmapped cause reports no member rather than leaking one.
const REJECTION_MEMBERS: &[(&str, AgentPromptRejection)] = &[
    ("agent is blocked", AgentPromptRejection::Blocked),
    (
        "agent status source is not integration",
        AgentPromptRejection::NotPromptable,
    ),
    (
        "agent state does not admit prompts",
        AgentPromptRejection::NotPromptable,
    ),
    (
        "agent is not the terminal foreground process",
        AgentPromptRejection::NotForeground,
    ),
    (
        "agent status is unavailable",
        AgentPromptRejection::FenceChanged,
    ),
    (
        "agent status fence changed",
        AgentPromptRejection::FenceChanged,
    ),
    (
        "agent process proof could not be refreshed",
        AgentPromptRejection::ProcessChanged,
    ),
    (
        "agent process proof changed before prompt admission",
        AgentPromptRejection::ProcessChanged,
    ),
    (
        "agent process proof changed before the keeper write",
        AgentPromptRejection::ProcessChanged,
    ),
    (
        "session unavailable",
        AgentPromptRejection::SessionUnavailable,
    ),
    (
        "session is not live",
        AgentPromptRejection::SessionUnavailable,
    ),
    (
        "session changed before prompt admission",
        AgentPromptRejection::SessionUnavailable,
    ),
    (
        "session changed before the keeper write",
        AgentPromptRejection::SessionUnavailable,
    ),
    (
        "terminal input mode could not be read",
        AgentPromptRejection::SessionUnavailable,
    ),
    (
        "worker connection was superseded",
        AgentPromptRejection::SessionUnavailable,
    ),
    ("unknown session", AgentPromptRejection::SessionUnavailable),
    (
        "worker unavailable",
        AgentPromptRejection::SessionUnavailable,
    ),
    (
        "coordinator keeper update preparation in progress",
        AgentPromptRejection::SessionUnavailable,
    ),
    (
        "coordinator write lease unavailable",
        AgentPromptRejection::SessionUnavailable,
    ),
    ("prompt budget expired", AgentPromptRejection::Expired),
    (
        "prompt budget could not be verified",
        AgentPromptRejection::Expired,
    ),
    (
        "prompt budget cannot cover the submit delay",
        AgentPromptRejection::Expired,
    ),
    (
        "input budget expired before worker send",
        AgentPromptRejection::Expired,
    ),
    (
        "keeper rejected the agent prompt",
        AgentPromptRejection::KeeperRejected,
    ),
    (
        "keeper did not admit the agent prompt",
        AgentPromptRejection::KeeperRejected,
    ),
    (
        "keeper rejected input",
        AgentPromptRejection::KeeperRejected,
    ),
    (
        "prompt admission could not be verified",
        AgentPromptRejection::KeeperRejected,
    ),
    (
        "generation closed or control queue full",
        AgentPromptRejection::KeeperRejected,
    ),
];

/// A request whose every field passed the public bounds.
struct ValidatedAgentPrompt {
    session_id: SessionId,
    expected_status_epoch: StatusEpoch,
    expected_occupant_id: AgentOccupantId,
    expected_revision: i64,
    text: String,
    wait: Option<AgentPromptWaitConfig>,
}

/// `CoordinatorService.SessionsPrompt` -- one fenced prompt, optionally waited on.
pub async fn handle_sessions_prompt(
    core: &CoordCore,
    caller: &Caller,
    request: SessionsPromptRequest,
) -> ServiceResult<SessionsPromptResponse> {
    let browser_fp = require_account_device(caller)?;
    let validated = validate_agent_prompt_request(request)?;
    let open: Option<(String,)> =
        sqlx::query_as("SELECT id FROM sessions WHERE id = $1 AND status = 'open'")
            .bind(validated.session_id.as_str())
            .fetch_optional(core.services.db.pool())
            .await
            .map_err(storage_failed)?;
    if open.is_none() {
        let rejected =
            terminal_write_rejected(validated.session_id.as_str(), 0, "session unavailable");
        return ok_response(prompt_response(&rejected, None));
    }
    let services = &core.services;
    let command = AgentPromptControlCommand {
        identity: terminal_viewer_identity(browser_fp, caller.tab_id.as_deref()),
        session_id: validated.session_id,
        input_seq: services.terminal_input.next_compatibility_input_seq(),
        expected_status_epoch: validated.expected_status_epoch,
        expected_occupant_id: validated.expected_occupant_id,
        expected_revision: validated.expected_revision,
        text: validated.text,
        wait: validated.wait,
        deadline: None,
    };
    let session_id = command.session_id.clone();
    let status_epoch = command.expected_status_epoch.clone();
    let occupant_id = command.expected_occupant_id.clone();
    let result = process_agent_prompt_control(services, command)
        .await
        .map_err(remap_agent_prompt_wait_error)?;
    roost_observability::log::info(
        "agents.prompt",
        "completed",
        LogFields::new()
            .set("session_id", session_id.as_str())
            .set("status_epoch", status_epoch.as_str())
            .set("occupant_id", occupant_id.as_str())
            .set("input_outcome", result.input.status.as_str())
            .set(
                "wait_outcome",
                result
                    .wait_outcome
                    .map_or("", AgentPromptWaitOutcome::as_str),
            ),
    );
    ok_response(prompt_response(&result.input, result.wait_outcome))
}

fn validate_agent_prompt_request(
    request: SessionsPromptRequest,
) -> Result<ValidatedAgentPrompt, ConnectError> {
    let invalid = || ConnectError::new(ErrorCode::InvalidArgument, "invalid agent prompt request");
    let session_id = SessionId::try_from(request.session_id.as_str()).map_err(|_| invalid())?;
    let expected_status_epoch =
        StatusEpoch::try_from(request.expected_status_epoch.as_str()).map_err(|_| invalid())?;
    let expected_occupant_id =
        AgentOccupantId::try_from(request.expected_occupant_id.as_str()).map_err(|_| invalid())?;
    if request.expected_revision > MAX_SAFE_INTEGER || !is_valid_agent_prompt_text(&request.text) {
        return Err(invalid());
    }
    let has_wait_states = !request.wait_states.is_empty();
    if has_wait_states != request.wait_timeout_ms.is_some() {
        return Err(invalid());
    }
    let unique: HashSet<&str> = request.wait_states.iter().map(String::as_str).collect();
    if unique.len() != request.wait_states.len()
        || !request
            .wait_states
            .iter()
            .all(|state| matches!(state.as_str(), "working" | "blocked" | "idle"))
    {
        return Err(invalid());
    }
    let wait = match request.wait_timeout_ms {
        Some(timeout_ms)
            if !(AGENT_PROMPT_WAIT_TIMEOUT_MIN_MS..=AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS)
                .contains(&i64::from(timeout_ms)) =>
        {
            return Err(invalid());
        }
        Some(timeout_ms) => Some(AgentPromptWaitConfig {
            states: request.wait_states,
            timeout_ms: u64::from(timeout_ms),
        }),
        None => None,
    };
    Ok(ValidatedAgentPrompt {
        session_id,
        expected_status_epoch,
        expected_occupant_id,
        expected_revision: request.expected_revision as i64,
        text: request.text,
        wait,
    })
}

fn prompt_response(
    input: &TerminalWriteControlResult,
    wait_outcome: Option<AgentPromptWaitOutcome>,
) -> SessionsPromptResponse {
    let (input_outcome, reason) = match input.status {
        TerminalWriteStatus::Accepted => (AgentPromptInputOutcome::Accepted, ""),
        TerminalWriteStatus::Rejected => {
            (AgentPromptInputOutcome::Rejected, "agent prompt rejected")
        }
        TerminalWriteStatus::Ambiguous => (
            AgentPromptInputOutcome::Ambiguous,
            "agent prompt outcome is ambiguous",
        ),
    };
    let rejected = input.status == TerminalWriteStatus::Rejected;
    SessionsPromptResponse {
        input_outcome: EnumValue::Known(input_outcome),
        written_bytes: input.written_bytes,
        reason: reason.to_owned(),
        wait_outcome: wait_outcome
            .filter(|_| !rejected)
            .map(|outcome| EnumValue::Known(wait_member(outcome))),
        rejection: if rejected {
            rejection_member(&input.reason, &input.session_id).map(EnumValue::Known)
        } else {
            None
        },
        ..Default::default()
    }
}

fn wait_member(outcome: AgentPromptWaitOutcome) -> WireWaitOutcome {
    match outcome {
        AgentPromptWaitOutcome::Status(AgentStatusWaitOutcome::Matched { .. }) => {
            WireWaitOutcome::Matched
        }
        AgentPromptWaitOutcome::Status(AgentStatusWaitOutcome::TimedOut) => {
            WireWaitOutcome::TimedOut
        }
        AgentPromptWaitOutcome::Status(AgentStatusWaitOutcome::OccupantChanged) => {
            WireWaitOutcome::OccupantChanged
        }
        AgentPromptWaitOutcome::Status(AgentStatusWaitOutcome::SessionClosed) => {
            WireWaitOutcome::SessionClosed
        }
        AgentPromptWaitOutcome::PromptStalled => WireWaitOutcome::PromptStalled,
    }
}

fn rejection_member(reason: &str, session_id: &str) -> Option<AgentPromptRejection> {
    let member = REJECTION_MEMBERS
        .iter()
        .find(|(known, _)| *known == reason)
        .map(|(_, member)| *member);
    // The reason itself is private, so record only that a cause went unclassified.
    if member.is_none() {
        roost_observability::log::warn(
            "agents.prompt",
            "rejection_unmapped",
            LogFields::new()
                .set("session_id", session_id)
                .set("reason_length", reason.len() as u64),
        );
    }
    member
}

fn remap_agent_prompt_wait_error(error: AgentStatusWaitError) -> ConnectError {
    let code = match error.kind() {
        AgentStatusWaitErrorKind::Invalid => ErrorCode::InvalidArgument,
        AgentStatusWaitErrorKind::Capacity => ErrorCode::ResourceExhausted,
        AgentStatusWaitErrorKind::Canceled => ErrorCode::Canceled,
    };
    ConnectError::new(code, error.message())
}

fn storage_failed(error: sqlx::Error) -> ConnectError {
    roost_observability::log::error(
        "agents.prompt",
        "storage_failed",
        LogFields::new().set("error", error.to_string()),
    );
    ConnectError::new(ErrorCode::Internal, "agent prompt storage failed")
}
