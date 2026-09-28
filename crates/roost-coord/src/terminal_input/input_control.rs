//! One logical PTY input batch, routed exactly once through its sender's lane,
//! and -- for audited Sync input -- reported only after its audit row is
//! durable. Once the worker transport admits a batch, a missing or malformed
//! result is ambiguous and is never retried. Called by the Sync terminal
//! controls and the unary `SessionsInput` handler.
//! Ports `apps/coord/src/terminal/input/input-control.ts`.

use std::sync::Arc;

use roost_protocol::wire::{SessionId, WorkerFp};

use crate::services::CoordServices;
use crate::terminal_input::control_lane::TerminalViewerIdentity;
use crate::terminal_input::input_audit::InputAuditEntry;
use crate::terminal_input::write_control::{
    TerminalWriteAcceptance, TerminalWriteControlCommand, TerminalWriteControlResult,
    TerminalWriteStatus, bounded_reason, process_terminal_write_control, terminal_write_rejected,
};
use crate::workers::hop_deadline::HopDeadline;
use crate::workers::terminal_send::{TerminalInputSend, send_terminal_input_request};

/// The largest batch one input command may carry.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;

/// The authenticated browser actor and route epoch a Sync batch carries to
/// the worker, which fences a batch from a route the browser has since left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRouteAuthority {
    /// The authenticated device.
    pub device_fingerprint: String,
    /// The authenticated tab.
    pub tab_id: String,
    /// The Sync socket the batch arrived on.
    pub connection_id: String,
    /// The route epoch the browser wrote under; empty without a claimed route.
    pub input_route_epoch: String,
}

/// One input batch.
#[derive(Debug, Clone)]
pub struct InputControlCommand {
    /// The sender whose lane the batch queues in.
    pub identity: TerminalViewerIdentity,
    /// The session written to.
    pub session_id: String,
    /// The batch's own sequence.
    pub input_seq: u64,
    /// The bytes.
    pub data: Vec<u8>,
    /// The Sync socket the batch arrived on; `None` for unary input.
    pub socket_generation: Option<String>,
    /// Present only for Sync input. Unary writers stay outside browser route
    /// ownership and carry the empty envelope.
    pub input_route_authority: Option<InputRouteAuthority>,
    /// Whether the outcome must be durably audited before it is reported.
    pub audited: bool,
    /// The hop budget; production starts it at entry.
    pub deadline: Option<HopDeadline>,
}

/// Route one input batch. Its lane place is taken before this returns, so a
/// socket's batches enter the FIFO in the order the socket delivered them.
pub fn process_input_control(
    services: &Arc<CoordServices>,
    command: InputControlCommand,
) -> impl Future<Output = TerminalWriteControlResult> + Send + 'static {
    let caller_fingerprint = command.identity.caller_fingerprint.clone();
    let audited = command.audited;
    let admission = admit_input(services, command);
    let services = Arc::clone(services);
    async move {
        let outcome = match admission {
            InputAdmission::Settled(outcome) => outcome,
            InputAdmission::Writing(write) => write.await,
        };
        if !audited {
            return outcome;
        }
        let entry = InputAuditEntry {
            caller_fingerprint,
            status: outcome.status,
            written_bytes: outcome.written_bytes,
        };
        match services
            .terminal_input
            .audit()
            .persist(&services, entry)
            .await
        {
            Ok(()) => outcome,
            Err(error) => audit_failed(outcome, &error),
        }
    }
}

/// Where a batch stands once its synchronous checks ran.
enum InputAdmission<Write> {
    /// Decided without a worker: empty, oversized, or naming no session.
    Settled(TerminalWriteControlResult),
    /// Queued in its lane; the future carries it the rest of the way.
    Writing(Write),
}

fn admit_input(
    services: &Arc<CoordServices>,
    command: InputControlCommand,
) -> InputAdmission<impl Future<Output = TerminalWriteControlResult> + Send + 'static> {
    if command.data.is_empty() {
        return InputAdmission::Settled(TerminalWriteControlResult {
            status: TerminalWriteStatus::Accepted,
            session_id: command.session_id,
            input_seq: command.input_seq,
            written_bytes: 0,
            reason: String::new(),
        });
    }
    let Some(written_bytes) = u32::try_from(command.data.len())
        .ok()
        .filter(|len| *len as usize <= MAX_INPUT_BYTES)
    else {
        let rejected = terminal_write_rejected(
            &command.session_id,
            command.input_seq,
            "input exceeds 64 KiB",
        );
        return InputAdmission::Settled(rejected);
    };
    let Ok(session) = SessionId::try_from(command.session_id.as_str()) else {
        let rejected =
            terminal_write_rejected(&command.session_id, command.input_seq, "unknown session");
        return InputAdmission::Settled(rejected);
    };
    let authority = command
        .input_route_authority
        .unwrap_or(InputRouteAuthority {
            device_fingerprint: String::new(),
            tab_id: String::new(),
            connection_id: String::new(),
            input_route_epoch: String::new(),
        });
    let message = TerminalInputSend {
        session_id: session,
        input_seq: command.input_seq,
        data: command.data,
        device_fingerprint: authority.device_fingerprint,
        tab_id: authority.tab_id,
        browser_connection_id: authority.connection_id,
        input_route_epoch: authority.input_route_epoch,
    };
    let write = TerminalWriteControlCommand {
        identity: command.identity,
        session_id: command.session_id,
        input_seq: command.input_seq,
        socket_generation: command.socket_generation,
        deadline: command.deadline,
    };
    let relay_services = Arc::clone(services);
    let send = move |worker_fp: &WorkerFp, deadline: HopDeadline| {
        send_terminal_input_request(&relay_services.scrollback, worker_fp, message, deadline)
    };
    let acceptance = TerminalWriteAcceptance::ExactBytes { written_bytes };
    InputAdmission::Writing(process_terminal_write_control(
        services, write, acceptance, send,
    ))
}

/// An outcome whose audit row could not be written. A rejection stays a
/// rejection -- nothing was written either way -- but anything else can no
/// longer be reported as proven.
fn audit_failed(outcome: TerminalWriteControlResult, error: &str) -> TerminalWriteControlResult {
    let reason = bounded_reason(&format!("input audit persistence failed: {error}"));
    tracing::warn!(session_id = %outcome.session_id, input_seq = outcome.input_seq,
        status = outcome.status.as_str(), %reason, "terminal input outcome lost its audit row");
    if outcome.status == TerminalWriteStatus::Rejected {
        return TerminalWriteControlResult { reason, ..outcome };
    }
    TerminalWriteControlResult {
        status: TerminalWriteStatus::Ambiguous,
        reason,
        ..outcome
    }
}
