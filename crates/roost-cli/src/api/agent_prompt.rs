//! `roost api agent-prompt`: send one prompt to the agent occupying a session,
//! fenced on the exact occupant the coordinator last reported. Called by
//! `api::mod`; depends on the generated `SessionsPrompt` method and the
//! generated outcome enums, on `roost-protocol`'s prompt byte caps, and on
//! `api::agent_projection`.
//!
//! WHY THE PROMPT IS FENCED ON AN OCCUPANT RATHER THAN SENT AT A SESSION. A
//! session's agent is a process that starts and stops. Between reading the
//! status and writing the bytes, that process can be replaced, and a prompt
//! aimed at the wrong one is text delivered into somebody else's terminal. So
//! the fence triple travels with the write and the coordinator refuses a
//! mismatch — which is why this verb reads the status first even when the
//! caller did not ask to wait.
//!
//! WHY THE OUTCOMES ARE MATCHED AS ENUM VARIANTS AND NOT AS NUMBERS. The
//! generated enums are the contract; a `match` over them cannot name a value
//! that does not exist, and adding a variant to the `.proto` turns the omission
//! into a compile error here. The wire numbers are never written in this file.

use std::process::ExitCode;

use roost_proto::{
    AgentPromptInputOutcome, AgentPromptRejection, AgentPromptWaitOutcome, AgentStatusGetRequest,
    SessionsPromptRequest,
};
use roost_protocol::terminal_input::{
    AGENT_PROMPT_MAX_TEXT_BYTES, AGENT_PROMPT_MAX_WRITE_BYTES, is_valid_agent_prompt_text,
};
use roost_protocol::wire::agent_status::agent_status_identity;

use crate::api::agent_projection;
use crate::api::agents::{parse_duration, parse_states};
use crate::api::client::CoordinatorApi;
use crate::api::output::ApiOutput;
use crate::api::verbs::Invocation;
use crate::command_error::{CommandFailure, GENERIC_FAILURE};

/// Send one prompt, and optionally wait for the agent to reach a state.
pub async fn prompt(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let session = args.positional(0, "session")?;
    let text = args.positional(1, "text")?;
    if !is_valid_agent_prompt_text(text) {
        return Err(CommandFailure::usage(format!(
            "roost api agent-prompt: the text must be nonempty and at most \
             {AGENT_PROMPT_MAX_TEXT_BYTES} UTF-8 bytes"
        )));
    }
    let wait = wait_request(args)?;

    let mut current = api
        .answer(api.stub().agent_status_get(AgentStatusGetRequest {
            session_id: session.to_string(),
            ..Default::default()
        }))
        .await?;
    let view = current.status.take().ok_or_else(|| {
        CommandFailure::generic(format!(
            "the coordinator at {} returned no agent status for {session}",
            api.origin()
        ))
    })?;
    let status = agent_projection::fields(&view)?;
    let fence = agent_status_identity(&status).ok_or_else(|| {
        CommandFailure::usage(format!(
            "agent-prompt: the coordinator's status for {session} carries no occupant identity, \
             so a prompt cannot be fenced to it"
        ))
    })?;
    let revision = u64::try_from(status.revision).map_err(|_| {
        CommandFailure::generic("agent-prompt: the status revision is out of range")
    })?;

    let response = api
        .answer(
            api.stub().sessions_prompt(SessionsPromptRequest {
                session_id: session.to_string(),
                expected_status_epoch: fence.status_epoch.as_str().to_string(),
                expected_occupant_id: fence.occupant_id.as_str().to_string(),
                expected_revision: revision,
                text: text.to_string(),
                wait_states: wait
                    .as_ref()
                    .map_or_else(Vec::new, |wait| wait.states.clone()),
                wait_timeout_ms: wait.as_ref().map(|wait| wait.timeout_ms),
                ..Default::default()
            }),
        )
        .await?;

    let input = input_outcome(&response.input_outcome)?;
    if !written_bytes_fit(input, response.written_bytes) {
        return Err(invalid(
            "a written byte count that disagrees with the outcome",
        ));
    }
    let rejection = match &response.rejection {
        None => None,
        Some(code) if input == "rejected" => Some(rejection_name(code.as_known())?),
        Some(_) => return Err(invalid("a rejection for a write it says it accepted")),
    };
    let wait_outcome = match (response.wait_outcome.as_ref(), &wait, input) {
        (None, _, _) => None,
        (Some(_code), None, _) => {
            return Err(invalid(
                "a wait outcome for a prompt with no wait asked for",
            ));
        }
        (Some(_), Some(_), "rejected") => {
            return Err(invalid("a wait outcome for a prompt it says it rejected"));
        }
        (Some(code), Some(_), _) => Some(wait_outcome_name(code.as_known())?),
    };

    out.answer(&format!(
        "input\t{input}\t{}\t{}",
        response.written_bytes,
        rejection.unwrap_or("ok")
    ));
    if let Some(outcome) = &wait_outcome {
        out.answer(&format!("wait\t{outcome}"));
    }
    let settled = input == "accepted" && wait_outcome.unwrap_or("matched") == "matched";
    Ok(if settled {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(GENERIC_FAILURE)
    })
}

/// The wait a caller asked for, or `None`, and a refusal for a half-asked one.
struct WaitRequest {
    states: Vec<String>,
    timeout_ms: u32,
}

fn wait_request(args: &Invocation) -> Result<Option<WaitRequest>, CommandFailure> {
    let asked = args.has("--wait");
    let until = args.optional_value("--until");
    let timeout = args.optional_value("--timeout");
    if !asked && until.is_none() && timeout.is_none() {
        return Ok(None);
    }
    let (Some(until), Some(timeout)) = (until, timeout) else {
        return Err(CommandFailure::usage(
            "roost api agent-prompt: --wait, --until and --timeout must be given together",
        ));
    };
    if !asked {
        return Err(CommandFailure::usage(
            "roost api agent-prompt: --until and --timeout are the wait; name it with --wait",
        ));
    }
    Ok(Some(WaitRequest {
        states: parse_states(until, "agent-prompt")?,
        timeout_ms: parse_duration(timeout, "agent-prompt")?,
    }))
}

fn input_outcome(
    value: &roost_proto::buffa::EnumValue<AgentPromptInputOutcome>,
) -> Result<&'static str, CommandFailure> {
    match value.as_known() {
        Some(AgentPromptInputOutcome::Accepted) => Ok("accepted"),
        Some(AgentPromptInputOutcome::Rejected) => Ok("rejected"),
        Some(AgentPromptInputOutcome::Ambiguous) => Ok("ambiguous"),
        // The proto3 zero value is a real arm, not an absent one: a
        // coordinator that never set the field sends it, and it means the
        // same thing as a value from a newer build — this one cannot reason
        // about it. Refusing is the honest answer for both.
        Some(AgentPromptInputOutcome::AGENT_PROMPT_INPUT_OUTCOME_UNSPECIFIED) | None => {
            Err(invalid("an input outcome this build does not know"))
        }
    }
}

fn rejection_name(value: Option<AgentPromptRejection>) -> Result<&'static str, CommandFailure> {
    match value {
        Some(AgentPromptRejection::Blocked) => Ok("blocked"),
        Some(AgentPromptRejection::NotPromptable) => Ok("not_promptable"),
        Some(AgentPromptRejection::NotForeground) => Ok("not_foreground"),
        Some(AgentPromptRejection::FenceChanged) => Ok("fence_changed"),
        Some(AgentPromptRejection::ProcessChanged) => Ok("process_changed"),
        Some(AgentPromptRejection::SessionUnavailable) => Ok("session_unavailable"),
        Some(AgentPromptRejection::Expired) => Ok("expired"),
        Some(AgentPromptRejection::KeeperRejected) => Ok("keeper_rejected"),
        Some(AgentPromptRejection::AGENT_PROMPT_REJECTION_UNSPECIFIED) | None => {
            Err(invalid("a rejection cause this build does not know"))
        }
    }
}

fn wait_outcome_name(
    value: Option<AgentPromptWaitOutcome>,
) -> Result<&'static str, CommandFailure> {
    match value {
        Some(AgentPromptWaitOutcome::Matched) => Ok("matched"),
        Some(AgentPromptWaitOutcome::TimedOut) => Ok("timed_out"),
        Some(AgentPromptWaitOutcome::OccupantChanged) => Ok("occupant_changed"),
        Some(AgentPromptWaitOutcome::SessionClosed) => Ok("session_closed"),
        Some(AgentPromptWaitOutcome::PromptStalled) => Ok("prompt_stalled"),
        Some(AgentPromptWaitOutcome::AGENT_PROMPT_WAIT_OUTCOME_UNSPECIFIED) | None => {
            Err(invalid("a wait outcome this build does not know"))
        }
    }
}

/// An accepted write wrote something and a rejected write wrote nothing. A
/// coordinator describing anything else has reported a write this command
/// cannot reason about, and calling that success would be a lie about a prompt.
fn written_bytes_fit(input: &str, written: u32) -> bool {
    let written = written as usize;
    if written > AGENT_PROMPT_MAX_WRITE_BYTES {
        return false;
    }
    match input {
        "accepted" => written > 0,
        "rejected" => written == 0,
        _ => true,
    }
}

fn invalid(what: &str) -> CommandFailure {
    CommandFailure::generic(format!("agent-prompt: the coordinator returned {what}"))
}

#[cfg(test)]
mod tests {
    use super::{input_outcome, invalid, rejection_name, wait_outcome_name, written_bytes_fit};
    use crate::command_error::CommandFailure;
    use roost_proto::{
        AgentPromptInputOutcome, AgentPromptRejection, AgentPromptWaitOutcome, buffa::EnumValue,
    };

    #[test]
    fn an_outcome_this_build_does_not_know_is_refused_not_guessed() {
        let unknown = EnumValue::<AgentPromptInputOutcome>::Unknown(41);
        let failure = input_outcome(&unknown)
            .expect_err("a value outside the generated enum is a contract change");
        let CommandFailure { message, .. } = failure;
        assert!(message.contains("agent-prompt"), "{message}");
    }

    #[test]
    fn every_generated_rejection_and_wait_outcome_has_one_published_name() {
        let rejections = [
            AgentPromptRejection::Blocked,
            AgentPromptRejection::NotPromptable,
            AgentPromptRejection::NotForeground,
            AgentPromptRejection::FenceChanged,
            AgentPromptRejection::ProcessChanged,
            AgentPromptRejection::SessionUnavailable,
            AgentPromptRejection::Expired,
            AgentPromptRejection::KeeperRejected,
        ];
        for rejection in rejections {
            assert!(rejection_name(Some(rejection)).is_ok());
        }
        let waits = [
            AgentPromptWaitOutcome::Matched,
            AgentPromptWaitOutcome::TimedOut,
            AgentPromptWaitOutcome::OccupantChanged,
            AgentPromptWaitOutcome::SessionClosed,
            AgentPromptWaitOutcome::PromptStalled,
        ];
        for wait in waits {
            assert!(wait_outcome_name(Some(wait)).is_ok());
        }
        assert!(rejection_name(None).is_err());
        assert!(wait_outcome_name(None).is_err());
    }

    #[test]
    fn a_refusal_names_the_verb_so_a_reader_kees_which_command_answered() {
        assert!(invalid("x").message.contains("agent-prompt"));
    }

    #[test]
    fn an_accepted_write_wrote_bytes_and_a_rejected_one_wrote_none() {
        assert!(written_bytes_fit("accepted", 1));
        assert!(!written_bytes_fit("accepted", 0));
        assert!(written_bytes_fit("rejected", 0));
        assert!(!written_bytes_fit("rejected", 4));
        assert!(!written_bytes_fit("ambiguous", u32::MAX));
    }
}
