//! One logical PTY write, routed exactly once: the sender/session FIFO, the
//! write-gate lease taken INSIDE it, the monotonic hop deadline, and the
//! classification of the worker's keeper-proven result. Once the worker
//! transport admits a write, its outcome can only be accepted or ambiguous --
//! the coordinator never retries it. Entered by `terminal_input::input_control`.
//! Ports `apps/coord/src/terminal/input/terminal-write-control.ts`.

use std::sync::Arc;

use roost_protocol::terminal_input::AGENT_PROMPT_MAX_REASON_LENGTH;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::{InputResult, TerminalInputStatus, TerminalWritePhase};

use crate::services::CoordServices;
use crate::terminal_input::control_lane::{TerminalViewerIdentity, resolve_session_route};
use crate::terminal_input::input_timings::{InputStage, InputStageClock};
use crate::workers::hop_deadline::{HopDeadline, INPUT_CONTROL_TIMEOUT_MS};
use crate::workers::terminal_request::TerminalWorkerRequest;

/// What the coordinator can truthfully say about one write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalWriteStatus {
    /// The keeper wrote exactly what was sent.
    Accepted,
    /// Nothing was written; the client may restore its draft and retry.
    Rejected,
    /// Some or all of it may have been written; it must not be retried.
    Ambiguous,
}

impl TerminalWriteStatus {
    /// The wire and audit spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// One write's outcome. A rejection always reports zero written bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalWriteControlResult {
    /// The classification.
    pub status: TerminalWriteStatus,
    /// The session the write addressed.
    pub session_id: String,
    /// The write's own sequence.
    pub input_seq: u64,
    /// Bytes the keeper proved written, bounded by what was sent.
    pub written_bytes: u32,
    /// Why, for a rejection or an ambiguity; empty when accepted.
    pub reason: String,
}

/// Who writes what, and on which Sync generation.
#[derive(Debug, Clone)]
pub struct TerminalWriteControlCommand {
    /// The sender whose lane the write queues in.
    pub identity: TerminalViewerIdentity,
    /// The session written to.
    pub session_id: String,
    /// The write's own sequence.
    pub input_seq: u64,
    /// The Sync socket the write arrived on; `None` for unary input, which
    /// has no generation to cancel.
    pub socket_generation: Option<String>,
    /// The budget shared by the lane wait and the worker hop. Production
    /// starts it at entry; a test injects one.
    pub deadline: Option<HopDeadline>,
    /// Stage marks for the settle log; `None` when nobody reads them.
    pub stage_clock: Option<InputStageClock>,
}

/// What a worker result must prove for the write to count as accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalWriteAcceptance {
    /// Raw input: the keeper wrote exactly this many bytes.
    ExactBytes {
        /// The batch length.
        written_bytes: u32,
    },
    /// A worker-built write (the fenced agent prompt of
    /// `agents::prompt_control`): the worker proves the `Written` phase and a
    /// non-zero byte count no larger than the cap, since only it knows the
    /// final bytes it put on the PTY.
    WorkerWritten {
        /// The largest write the worker may report.
        maximum_written_bytes: u32,
    },
}

/// A definite pre-write refusal.
#[must_use]
pub fn terminal_write_rejected(
    session_id: &str,
    input_seq: u64,
    reason: &str,
) -> TerminalWriteControlResult {
    TerminalWriteControlResult {
        status: TerminalWriteStatus::Rejected,
        session_id: session_id.to_owned(),
        input_seq,
        written_bytes: 0,
        reason: bounded_reason(reason),
    }
}

/// A reason capped at the length a client displays.
#[must_use]
pub fn bounded_reason(reason: &str) -> String {
    reason
        .chars()
        .take(AGENT_PROMPT_MAX_REASON_LENGTH)
        .collect()
}

/// Route one logical PTY write exactly once.
///
/// The lane place is taken NOW, synchronously, so writes enter their FIFO in
/// the order their socket delivered them; the returned future waits its turn,
/// leases, resolves the route and sends. The lease is taken inside the lane,
/// never before it: leasing earlier would let queued input hold the exclusive
/// keeper-update drain open forever.
pub fn process_terminal_write_control<Sender>(
    services: &Arc<CoordServices>,
    command: TerminalWriteControlCommand,
    acceptance: TerminalWriteAcceptance,
    send_to_worker: Sender,
) -> impl Future<Output = TerminalWriteControlResult> + Send + 'static
where
    Sender: FnOnce(&WorkerFp, HopDeadline) -> TerminalWorkerRequest<InputResult> + Send + 'static,
{
    let deadline = command
        .deadline
        .unwrap_or_else(|| HopDeadline::start(INPUT_CONTROL_TIMEOUT_MS));
    let ticket = services.terminal_input.lanes().enqueue(
        &command.identity.viewer_key,
        &command.session_id,
        command.socket_generation.as_deref(),
    );
    let services = Arc::clone(services);
    async move {
        let reject =
            |reason: &str| terminal_write_rejected(&command.session_id, command.input_seq, reason);
        let Some(mut ticket) = ticket else {
            return reject("generation closed or control queue full");
        };
        if !ticket.wait_turn().await {
            return reject("generation closed or control queue full");
        }
        let _lease = match services.write_gate.acquire_shared() {
            Ok(lease) => lease,
            Err(error) => return reject(&error.to_string()),
        };
        let mark = |stage: InputStage| {
            if let Some(clock) = &command.stage_clock {
                clock.mark(stage);
            }
        };
        let resolved =
            resolve_session_route(&services.db, &services.byte_hub, &command.session_id).await;
        mark(InputStage::Routed);
        let route = match resolved {
            Ok(Some(route)) => route,
            Ok(None) => return reject("unknown session"),
            Err(error) => return reject(&error.to_string()),
        };
        let request = send_to_worker(&route.worker_fp, deadline);
        mark(InputStage::Sent);
        if !request.is_admitted() {
            return reject(if request.is_expired() {
                "input budget expired before worker send"
            } else {
                "worker unavailable"
            });
        }
        // Socket order is fixed at admission. The worker's keeper lane owns the
        // remaining FIFO while this write waits for its proof.
        ticket.release_lane();
        let worker_result = request.result().await;
        mark(InputStage::Settled);
        match worker_result {
            Ok(result)
                if result.session_id.as_str() != command.session_id
                    || result.input_seq != command.input_seq =>
            {
                ambiguous(&command, 0, "mismatched worker input result")
            }
            Ok(result) => classify_worker_result(&command, &result, acceptance),
            Err(error) => {
                let reason = error
                    .message
                    .as_deref()
                    .unwrap_or("input result unavailable");
                ambiguous(&command, 0, reason)
            }
        }
    }
}

fn classify_worker_result(
    command: &TerminalWriteControlCommand,
    result: &InputResult,
    acceptance: TerminalWriteAcceptance,
) -> TerminalWriteControlResult {
    let (maximum, accepted) = match acceptance {
        // Raw input predates write-phase proof and keeps its exact-byte rule.
        TerminalWriteAcceptance::ExactBytes {
            written_bytes: sent,
        } => (sent, result.written_bytes == sent),
        TerminalWriteAcceptance::WorkerWritten {
            maximum_written_bytes,
        } => (
            maximum_written_bytes,
            result.phase == TerminalWritePhase::Written
                && result.written_bytes > 0
                && result.written_bytes <= maximum_written_bytes,
        ),
    };
    let written_bytes = result.written_bytes.min(maximum);
    if result.status == TerminalInputStatus::Accepted && accepted {
        return TerminalWriteControlResult {
            status: TerminalWriteStatus::Accepted,
            session_id: command.session_id.clone(),
            input_seq: command.input_seq,
            written_bytes,
            reason: String::new(),
        };
    }
    if result.status == TerminalInputStatus::Rejected
        && result.phase == TerminalWritePhase::PreWrite
        && result.written_bytes == 0
    {
        let reason = if result.reason.is_empty() {
            "keeper rejected input"
        } else {
            &result.reason
        };
        return terminal_write_rejected(&command.session_id, command.input_seq, reason);
    }
    let reason = if result.reason.is_empty() {
        "input completion could not be proven"
    } else {
        &result.reason
    };
    ambiguous(command, written_bytes, reason)
}

fn ambiguous(
    command: &TerminalWriteControlCommand,
    written_bytes: u32,
    reason: &str,
) -> TerminalWriteControlResult {
    TerminalWriteControlResult {
        status: TerminalWriteStatus::Ambiguous,
        session_id: command.session_id.clone(),
        input_seq: command.input_seq,
        written_bytes,
        reason: bounded_reason(reason),
    }
}
