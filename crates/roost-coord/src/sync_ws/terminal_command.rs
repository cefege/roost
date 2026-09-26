//! The terminal command gate: the fence every terminal command passes before it
//! can reach a worker, and the definite answer a refused one owes the client.
//!
//! Owned by the Sync session; `commands.rs` owns the socket-identity fence and
//! the domain commands. Split out because a terminal command has a different
//! KIND of answer from a domain command: a refused `input` must carry a reason
//! AND the command's own domain generation, and getting that wrong is the
//! difference between a client that restores a draft and a client that reports
//! possible input loss (`docs/FAILURE-INDEX.md`, "A terminal domain reset is
//! treated as the input fence").

use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::{FirehoseFrame, SyncDomain};

use crate::sync_ws::control_frames::{
    input_rejected_frame, input_route_refusal_frame, transport_probe_refusal_frame,
};
use crate::sync_ws::session::SyncV2Session;

use super::commands::{ClientContext, CommandOutcome, TerminalCommand};

/// The terminal command gate: every terminal command is fenced before it can
/// reach a worker.
pub(in crate::sync_ws) fn terminal_command_gate(
    session: &mut SyncV2Session,
    context: &ClientContext,
    command: &Command,
) -> CommandOutcome {
    let (generation, refusal) = match command {
        Command::TerminalTransportProbe(_probe) => {
            let refusal = context
                .read_only
                .then_some("this Sync socket cannot write terminal input");
            (None, refusal)
        }
        Command::TerminalView(view) => (
            Some(view.domain_generation),
            terminal_refusal(session, context.read_only, view.domain_generation),
        ),
        Command::TerminalResync(resync) => (
            Some(resync.domain_generation),
            terminal_refusal(session, context.read_only, resync.domain_generation),
        ),
        Command::Input(input) => (
            Some(input.domain_generation),
            terminal_refusal(session, context.read_only, input.domain_generation),
        ),
        Command::InputRouteClaim(claim) => (
            Some(claim.domain_generation),
            terminal_refusal(session, context.read_only, claim.domain_generation),
        ),
        Command::DomainReady(_)
        | Command::DomainSubscribe(_)
        | Command::DomainUnsubscribe(_)
        | Command::UiApplyLayoutResult(_) => {
            return CommandOutcome::Nothing;
        }
    };
    if let Some(reason) = refusal {
        return match refusal_frame(command, reason) {
            Some(frame) => CommandOutcome::Refusal(frame),
            // A refused view or resync has no wire answer of its own: the client
            // learns the refusal from the view-state the hub would have sent.
            None => CommandOutcome::Nothing,
        };
    }
    let terminal = terminal_command_of(command);
    let _ = generation;
    match terminal {
        Some(command) => CommandOutcome::Terminal(command),
        None => CommandOutcome::Nothing,
    }
}

/// Why a terminal command cannot be honoured on this socket, or `None`.
fn terminal_refusal(
    session: &SyncV2Session,
    read_only: bool,
    domain_generation: u64,
) -> Option<&'static str> {
    if read_only {
        return Some("this Sync socket cannot write terminal input");
    }
    if !session.terminal_domain_ready() {
        return Some("terminal domain is resubscribing; input was not sent");
    }
    let current = session.domain_generation(SyncDomain::Terminal)?;
    if domain_generation != current {
        return Some("terminal view generation was reset; input was not sent");
    }
    None
}

fn terminal_command_of(command: &Command) -> Option<TerminalCommand> {
    match command {
        Command::TerminalView(view) => Some(TerminalCommand::View(view.as_ref().clone())),
        Command::TerminalResync(resync) => Some(TerminalCommand::Resync(resync.as_ref().clone())),
        Command::Input(input) => Some(TerminalCommand::Input(input.as_ref().clone())),
        Command::InputRouteClaim(claim) => {
            Some(TerminalCommand::RouteClaim(claim.as_ref().clone()))
        }
        Command::TerminalTransportProbe(probe) => {
            Some(TerminalCommand::TransportProbe(probe.as_ref().clone()))
        }
        Command::DomainReady(_)
        | Command::DomainSubscribe(_)
        | Command::DomainUnsubscribe(_)
        | Command::UiApplyLayoutResult(_) => None,
    }
}

/// The definite answer a refused terminal command owes the client.
fn refusal_frame(command: &Command, reason: &str) -> Option<FirehoseFrame> {
    match command {
        Command::Input(input) => Some(input_rejected_frame(
            &input.session_id,
            input.input_seq,
            input.domain_generation,
            reason,
        )),
        Command::InputRouteClaim(claim) => Some(input_route_refusal_frame(
            &claim.request_id,
            &claim.session_id,
            claim.revision,
            &claim.worker_epoch,
            reason,
        )),
        Command::TerminalTransportProbe(probe) => Some(transport_probe_refusal_frame(
            &probe.request_id,
            &probe.worker_fp,
        )),
        Command::TerminalView(_) | Command::TerminalResync(_) => None,
        Command::DomainReady(_)
        | Command::DomainSubscribe(_)
        | Command::DomainUnsubscribe(_)
        | Command::UiApplyLayoutResult(_) => None,
    }
}
