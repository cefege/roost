//! Status-fenced agent-prompt orchestration: register the exact-occupant wait
//! BEFORE the write enters its FIFO, route one dedicated worker request, then
//! settle the wait. A settled-state wait is two-phase: observed activity is
//! required first, so a prompt the agent never processed reports a stall
//! instead of a match. Shares the raw-input lane, write gate, hop deadline and
//! worker-result classifier of `terminal_input::write_control`.
//! Called by `agents::rpc_prompt`. Ports `apps/coord/src/agents/agent-prompt-control.ts`.
//!
//! THE FENCE ITSELF IS THE WORKER'S. Epoch, occupant, revision and process
//! proof are checked by the worker against its live occupant; a mismatch comes
//! back as a pre-write rejection whose reason `agents::rpc_prompt` classifies.
//! A wait is cancelled by dropping its waiter, which deregisters it, so every
//! early return here consumes the provisional waiter.

use std::sync::Arc;
use std::time::Duration;

use roost_protocol::terminal_input::AGENT_PROMPT_MAX_WRITE_BYTES;
use roost_protocol::wire::{AgentOccupantId, AgentRuntimeState, SessionId, StatusEpoch};

use crate::agents::status_wait::{
    AgentStatusWaitError, AgentStatusWaitOutcome, AgentStatusWaitRequest, AgentStatusWaiter,
};
use crate::services::CoordServices;
use crate::terminal_input::control_lane::TerminalViewerIdentity;
use crate::terminal_input::write_control::{
    TerminalWriteAcceptance, TerminalWriteControlCommand, TerminalWriteControlResult,
    TerminalWriteStatus, process_terminal_write_control,
};
use crate::workers::hop_deadline::{HopDeadline, INPUT_CONTROL_TIMEOUT_MS};
use crate::workers::terminal_send::{AgentPromptSend, send_agent_prompt_request};

/// A prompt must move its agent inside this window before a settled-state wait
/// is honoured; without it an idle agent satisfies the wait with the turn the
/// prompt was supposed to start.
pub const AGENT_PROMPT_EFFECT_TIMEOUT_MS: u64 = 5_000;

/// The states that prove a prompt started a turn.
const PROMPT_ACTIVITY_STATES: [&str; 2] = ["working", "blocked"];

/// How a prompt's wait ended: a status-wait outcome, or a stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPromptWaitOutcome {
    /// The status wait ended this way.
    Status(AgentStatusWaitOutcome),
    /// The prompt produced no working/blocked state inside the activity gate.
    PromptStalled,
}

impl AgentPromptWaitOutcome {
    /// The wire and log spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status(outcome) => outcome.as_str(),
            Self::PromptStalled => "prompt_stalled",
        }
    }
}

/// The caller's own validated wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPromptWaitConfig {
    /// The distinct states that satisfy the wait.
    pub states: Vec<String>,
    /// The whole wait window.
    pub timeout_ms: u64,
}

/// One validated prompt.
#[derive(Debug, Clone)]
pub struct AgentPromptControlCommand {
    /// The sender whose lane the write queues in.
    pub identity: TerminalViewerIdentity,
    /// The prompted session.
    pub session_id: SessionId,
    /// The coordinator-minted write sequence.
    pub input_seq: u64,
    /// The pinned status epoch.
    pub expected_status_epoch: StatusEpoch,
    /// The pinned occupant.
    pub expected_occupant_id: AgentOccupantId,
    /// The revision the caller read.
    pub expected_revision: i64,
    /// The prompt text.
    pub text: String,
    /// The optional state wait.
    pub wait: Option<AgentPromptWaitConfig>,
    /// Test injection; production starts one deadline at entry.
    pub deadline: Option<HopDeadline>,
}

/// The write's outcome and, when configured and not rejected, the wait's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPromptControlResult {
    /// The classified write.
    pub input: TerminalWriteControlResult,
    /// The wait outcome.
    pub wait_outcome: Option<AgentPromptWaitOutcome>,
}

/// The first phase's activity gate.
#[derive(Debug, Clone, Copy)]
struct PromptActivityGate {
    timeout_ms: u64,
    /// A gate timeout is a stall only when the whole window was available.
    stall_on_timeout: bool,
}

/// Execute exactly one prompt write and, when configured, return the first
/// exact-occupant state transition observed after the requested revision.
pub async fn process_agent_prompt_control(
    services: &Arc<CoordServices>,
    command: AgentPromptControlCommand,
) -> Result<AgentPromptControlResult, AgentStatusWaitError> {
    let deadline = command
        .deadline
        .unwrap_or_else(|| HopDeadline::start(INPUT_CONTROL_TIMEOUT_MS));
    let Some(wait) = command.wait.clone() else {
        let input = prompt_write(services, &command, deadline).await;
        return Ok(AgentPromptControlResult {
            input,
            wait_outcome: None,
        });
    };
    let budget = HopDeadline::start(wait.timeout_ms);
    let gate = prompt_activity_gate(services, &command, &wait, &budget);
    let first_states: Vec<String> = match gate {
        Some(_) => PROMPT_ACTIVITY_STATES
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        None => wait.states.clone(),
    };
    let first_timeout = gate.map_or(wait.timeout_ms, |gate| gate.timeout_ms);
    // Registration validates and admits capacity synchronously; nothing is
    // enqueued when it refuses.
    let waiter = register(
        services,
        &command,
        &first_states,
        command.expected_revision,
        first_timeout,
    )?;
    let first_timer = HopDeadline::start(first_timeout);
    let input = prompt_write(services, &command, deadline).await;
    if input.status == TerminalWriteStatus::Rejected {
        drop(waiter);
        return Ok(AgentPromptControlResult {
            input,
            wait_outcome: None,
        });
    }
    let first = settle_by(waiter, &first_timer).await?;
    let wait_outcome = match gate {
        None => AgentPromptWaitOutcome::Status(first),
        Some(gate) => settled_wait_outcome(services, &command, &wait, gate, first, &budget).await?,
    };
    Ok(AgentPromptControlResult {
        input,
        wait_outcome: Some(wait_outcome),
    })
}

/// A caller that waits for activity itself needs no gate, and an agent already
/// working or blocked has already proved it.
fn prompt_activity_gate(
    services: &CoordServices,
    command: &AgentPromptControlCommand,
    wait: &AgentPromptWaitConfig,
    budget: &HopDeadline,
) -> Option<PromptActivityGate> {
    if wait
        .states
        .iter()
        .any(|state| PROMPT_ACTIVITY_STATES.contains(&state.as_str()))
    {
        return None;
    }
    let pinned = services.agents.status.retained_occupant_state(
        &command.session_id,
        &command.expected_status_epoch,
        &command.expected_occupant_id,
    );
    if matches!(
        pinned,
        Some(AgentRuntimeState::Working | AgentRuntimeState::Blocked)
    ) {
        return None;
    }
    let remaining_ms = (budget.remaining_ms().floor() as i64).max(1) as u64;
    Some(PromptActivityGate {
        timeout_ms: AGENT_PROMPT_EFFECT_TIMEOUT_MS.min(remaining_ms),
        stall_on_timeout: remaining_ms > AGENT_PROMPT_EFFECT_TIMEOUT_MS,
    })
}

/// Second phase: the caller's own wait, floored at the observed activity so a
/// completion that predates the prompt cannot satisfy it.
async fn settled_wait_outcome(
    services: &CoordServices,
    command: &AgentPromptControlCommand,
    wait: &AgentPromptWaitConfig,
    gate: PromptActivityGate,
    activity: AgentStatusWaitOutcome,
    budget: &HopDeadline,
) -> Result<AgentPromptWaitOutcome, AgentStatusWaitError> {
    let matched_revision = match activity {
        AgentStatusWaitOutcome::TimedOut if gate.stall_on_timeout => {
            return Ok(AgentPromptWaitOutcome::PromptStalled);
        }
        AgentStatusWaitOutcome::Matched { matched_revision } => matched_revision,
        other => return Ok(AgentPromptWaitOutcome::Status(other)),
    };
    let remaining_ms = budget.remaining_ms().floor() as i64;
    if remaining_ms < 1 {
        return Ok(AgentPromptWaitOutcome::Status(
            AgentStatusWaitOutcome::TimedOut,
        ));
    }
    let after = command.expected_revision.max(matched_revision);
    let waiter = register(services, command, &wait.states, after, remaining_ms as u64)?;
    let outcome = settle_by(waiter, budget).await?;
    Ok(AgentPromptWaitOutcome::Status(outcome))
}

fn register(
    services: &CoordServices,
    command: &AgentPromptControlCommand,
    states: &[String],
    after_revision: i64,
    timeout_ms: u64,
) -> Result<AgentStatusWaiter, AgentStatusWaitError> {
    let request = AgentStatusWaitRequest::new(
        command.session_id.as_str(),
        command.expected_status_epoch.as_str(),
        command.expected_occupant_id.as_str(),
        states,
        Some(after_revision),
        timeout_ms,
    )?;
    services.agents.status.wait_for_agent_status(request)
}

/// Settle a waiter no later than `deadline`; elapsing drops (deregisters) it.
async fn settle_by(
    waiter: AgentStatusWaiter,
    deadline: &HopDeadline,
) -> Result<AgentStatusWaitOutcome, AgentStatusWaitError> {
    let remaining = Duration::from_secs_f64(deadline.remaining_ms().max(0.0) / 1_000.0);
    match tokio::time::timeout(remaining, waiter.settle()).await {
        Ok(settled) => settled,
        Err(_elapsed) => Ok(AgentStatusWaitOutcome::TimedOut),
    }
}

fn prompt_write(
    services: &Arc<CoordServices>,
    command: &AgentPromptControlCommand,
    deadline: HopDeadline,
) -> impl Future<Output = TerminalWriteControlResult> + Send + 'static {
    let write = TerminalWriteControlCommand {
        identity: command.identity.clone(),
        session_id: command.session_id.as_str().to_owned(),
        input_seq: command.input_seq,
        socket_generation: None,
        deadline: Some(deadline),
        stage_clock: None,
    };
    let message = AgentPromptSend {
        session_id: command.session_id.clone(),
        input_seq: command.input_seq,
        expected_status_epoch: command.expected_status_epoch.as_str().to_owned(),
        expected_occupant_id: command.expected_occupant_id.as_str().to_owned(),
        expected_revision: command.expected_revision as u64,
        text: command.text.clone(),
    };
    let sender_services = Arc::clone(services);
    process_terminal_write_control(
        services,
        write,
        TerminalWriteAcceptance::WorkerWritten {
            maximum_written_bytes: AGENT_PROMPT_MAX_WRITE_BYTES as u32,
        },
        move |worker_fp, worker_deadline| {
            send_agent_prompt_request(
                &sender_services.scrollback,
                worker_fp,
                message,
                worker_deadline,
            )
        },
    )
}
