//! What one authenticated client frame did, decided here and applied by the
//! caller.
//!
//! Owned by the Sync session. v2 kept this in `sync-ws-v2-commands.ts` plus
//! `sync-ws-client-ingress.ts`; both are here because both answer the same
//! question -- is this frame legal for THIS socket, and what does it change --
//! and splitting them in Rust would put the socket-identity fence on one side of
//! a boundary and the command gate on the other.
//!
//! EVERY REFUSAL IS A FRAME, NOT A SILENCE. A command this socket may not issue
//! gets a definite answer carrying a reason, because the client is holding a
//! deadline and a command with no answer is a command the client classifies as
//! ambiguous: possible data loss, a restored draft, and an error the user sees.
//! `docs/FAILURE-INDEX.md`, "A terminal domain reset is treated as the input
//! fence", is the entry that made this the rule rather than a nicety.
//!
//! THE SOCKET ID IS THE FIRST FENCE, BEFORE ANY COMMAND IS LOOKED AT. A browser
//! that redials leaves a socket whose in-flight commands must not mutate the new
//! one, and the only way to tell them apart is the opaque id the coordinator
//! minted. Checking the command first would let a stale socket's `domain_ready`
//! close a fence it never hydrated.

use std::collections::BTreeSet;

use roost_proto::buffa::Message;
use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::{
    FirehoseFrame, InputCommand, SyncClientFrame, SyncDomain, SyncDomainReadyCommand,
    SyncDomainSubscriptionCommand, TerminalInputRouteClaim, TerminalResyncCommand,
    TerminalTransportProbe, TerminalViewCommand, UiApplyLayoutResult,
};

use crate::sync_ws::admission::EnqueueOutcome;
use crate::sync_ws::control_frames::ResetNotice;
use crate::sync_ws::frame_meta::is_lazy_domain;
use crate::sync_ws::snapshot_registry::SnapshotTokenRegistry;
use crate::sync_ws::session::SyncV2Session;

/// What a Sync socket is allowed to do, resolved at upgrade.
#[derive(Debug, Clone, Default)]
pub struct ClientContext {
    /// Whether the socket may issue terminal commands at all. A worker principal
    /// may ACK and subscribe but never view or write.
    pub read_only: bool,
    /// The browser tab this socket speaks for, when it has one.
    pub tab_id: Option<String>,
    /// The `${fingerprint}:${tab}` key that owns socket-bound view handles.
    pub viewer_key: Option<String>,
    /// The verified key's fingerprint, for correlating a layout result.
    pub fingerprint: String,
    /// The sessions this socket may observe, as resolved at upgrade.
    pub session_ids: BTreeSet<String>,
}

/// A terminal command that passed the gate, for the caller's sink.
#[derive(Debug, Clone, PartialEq)]
pub enum TerminalCommand {
    /// Mount, resize, or activate a view handle.
    View(TerminalViewCommand),
    /// Ask for the baseline this view expects.
    Resync(TerminalResyncCommand),
    /// Terminal input for one session.
    Input(InputCommand),
    /// Claim the input route for one session.
    RouteClaim(TerminalInputRouteClaim),
    /// Ask which worker epoch a session's transport reports.
    TransportProbe(TerminalTransportProbe),
}

impl TerminalCommand {
    /// The oneof case name, for a log line and a sink that dispatches on it.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::View(_) => "terminalView",
            Self::Resync(_) => "terminalResync",
            Self::Input(_) => "input",
            Self::RouteClaim(_) => "inputRouteClaim",
            Self::TransportProbe(_) => "terminalTransportProbe",
        }
    }
}

/// What one client frame did.
#[derive(Debug)]
pub enum CommandOutcome {
    /// The frame was a bare acknowledgement, or named a socket this session is
    /// not. Neither changes coordinator state, and neither is an error: a
    /// redialled client still acknowledges its old socket until that socket dies.
    Nothing,
    /// A cumulative acknowledgement was applied.
    Acknowledged {
        /// How many in-flight records it released.
        released: u64,
    },
    /// A domain closed its snapshot/live gap. The caller must seed it: the feed
    /// owns the retained frames, and seeding before the client is ready is the
    /// gap this command exists to close.
    DomainReady {
        /// Which domain.
        domain: SyncDomain,
        /// For the terminal domain, the sessions this socket may now observe,
        /// from the one-time snapshot token. `None` for every other domain,
        /// which has no such fence.
        admitted_sessions: Option<BTreeSet<String>>,
    },
    /// The audit domain's subscription changed. The caller must start or stop
    /// the audit source, because audit traffic is install-wide and a socket that
    /// is not showing it must not be fed it.
    AuditSubscription {
        /// Whether the client now wants audit rows.
        subscribed: bool,
    },
    /// A layout acknowledgement for a browser tab.
    LayoutResult {
        /// The tab the result belongs to.
        tab_id: String,
        /// The coordinator's result, for the layout-apply owner to settle.
        result: Box<UiApplyLayoutResult>,
    },
    /// A terminal command that passed the gate.
    Terminal(TerminalCommand),
    /// Send this control frame and stop. The frame is the definite answer a
    /// refusal owes the client.
    Refusal(FirehoseFrame),
    /// The terminal domain must be reset: the one-time snapshot token was
    /// missing, already consumed, or named a socket that is gone.
    ResetTerminal(ResetNotice),
    /// The frame is not a legal client frame and the socket must close `1008`.
    ///
    /// Two frames land here and both are protocol violations rather than
    /// refusals: a frame from this socket that carries neither an
    /// acknowledgement nor a command, and a frame whose bytes are not the
    /// canonical encoding of what it decoded to. Ignoring either would leave the
    /// client waiting for an answer to a frame the coordinator never understood.
    Invalid,
}

/// Whether a decoded client frame is byte-for-byte the canonical encoding of
/// itself.
///
/// The transport decodes; this decides whether the decode is trustworthy. A
/// frame with trailing junk, a non-minimal varint, or an unknown field would
/// otherwise be accepted with the unknown parts silently dropped, which is how a
/// client and a coordinator end up disagreeing about what was sent
/// (`sync-ws-client-ingress.ts:44-52`).
#[must_use]
pub fn is_canonical_client_frame(frame: &SyncClientFrame, raw: &[u8]) -> bool {
    let mut encoded = Vec::new();
    if frame.try_encode(&mut encoded).is_err() {
        return false;
    }
    encoded == raw
}

/// Apply one client frame to one session.
///
/// `now_ms` is threaded in rather than read so the acknowledgement path and the
/// domain path agree on one instant, and so a test can place a frame in time
/// without a clock.
pub fn handle_client_frame(
    session: &mut SyncV2Session,
    context: &ClientContext,
    frame: &SyncClientFrame,
    tokens: &mut SnapshotTokenRegistry,
    now_ms: u64,
) -> CommandOutcome {
    if frame.socket_id != session.socket_id {
        return CommandOutcome::Nothing;
    }
    let ack = frame.ack_delivery_seq.unwrap_or_default();
    if ack > 0 {
        match session.apply_ack(ack, now_ms) {
            Ok(released) if released > 0 => return CommandOutcome::Acknowledged { released },
            Ok(_) => {}
            Err(close) => {
                session.fault(close);
                return CommandOutcome::Invalid;
            }
        }
    }
    match &frame.command {
        None => CommandOutcome::Invalid,
        Some(Command::DomainReady(ready)) => handle_domain_ready(session, context, ready, tokens),
        Some(Command::DomainSubscribe(subscribe)) => {
            set_lazy_domain_subscription(session, context, subscribe, true)
        }
        Some(Command::DomainUnsubscribe(unsubscribe)) => {
            set_lazy_domain_subscription(session, context, unsubscribe, false)
        }
        Some(Command::UiApplyLayoutResult(result)) => layout_result(context, result),
        Some(other) => super::terminal_command::terminal_command_gate(session, context, other),
    }
}

fn handle_domain_ready(
    session: &mut SyncV2Session,
    context: &ClientContext,
    ready: &SyncDomainReadyCommand,
    tokens: &mut SnapshotTokenRegistry,
) -> CommandOutcome {
    // A domain value this build does not know is a protocol violation, not a
    // command for another domain: the client named something that cannot exist,
    // and answering `Nothing` would leave it believing the fence closed.
    let Some(domain) = ready.domain.as_known() else {
        return CommandOutcome::Invalid;
    };
    let Some(state) = session.domain(domain) else {
        return CommandOutcome::Nothing;
    };
    if !state.subscribed || ready.generation != state.generation || state.ready {
        return CommandOutcome::Nothing;
    }
    let mut admitted_sessions = None;
    if domain == SyncDomain::Terminal {
        // The terminal domain's fence is a one-time token, and a token that does
        // not check out RESETS the domain rather than being ignored: a client
        // that believes the terminal domain is hydrated and is not would
        // otherwise receive cells for sessions it never learned about.
        let Some(token) = ready.snapshot_token.as_deref() else {
            return reset_terminal(session, "snapshot_token_invalid");
        };
        let Some(covered) = tokens.consume(&session.socket_id, token) else {
            return reset_terminal(session, "snapshot_token_invalid");
        };
        // A snapshot may cover a session this socket's scope has since lost, so
        // the intersection is what gets admitted.
        let admitted: BTreeSet<String> = covered
            .into_iter()
            .filter(|session_id| context.session_ids.contains(session_id))
            .collect();
        session.announced_sessions.clear();
        session.pending_session_announcements.clear();
        session
            .announced_sessions
            .extend(admitted.iter().cloned());
        admitted_sessions = Some(admitted);
    }
    if let Some(state) = session.domain_mut(domain) {
        state.ready = true;
    }
    session.request_flush();
    CommandOutcome::DomainReady {
        domain,
        admitted_sessions,
    }
}

fn reset_terminal(session: &mut SyncV2Session, reason: &'static str) -> CommandOutcome {
    match session.reset_domain(SyncDomain::Terminal, reason) {
        EnqueueOutcome::Reset(notice) => CommandOutcome::ResetTerminal(notice),
        _ => CommandOutcome::Invalid,
    }
}

fn set_lazy_domain_subscription(
    session: &mut SyncV2Session,
    context: &ClientContext,
    command: &SyncDomainSubscriptionCommand,
    subscribe: bool,
) -> CommandOutcome {
    let Some(domain) = command.domain.as_known() else {
        return CommandOutcome::Invalid;
    };
    if !is_lazy_domain(domain) {
        return CommandOutcome::Nothing;
    }
    let Some(state) = session.domain(domain) else {
        return CommandOutcome::Nothing;
    };
    if command.generation != state.generation {
        return CommandOutcome::Nothing;
    }
    if subscribe {
        if state.subscribed {
            return CommandOutcome::Nothing;
        }
        if let Some(state) = session.domain_mut(domain) {
            state.subscribed = true;
            state.ready = false;
        }
    } else {
        if !state.subscribed {
            return CommandOutcome::Nothing;
        }
        session.clear_domain_queue(domain);
        let generation = session.allocate_generation();
        if let Some(state) = session.domain_mut(domain) {
            state.subscribed = false;
            state.ready = false;
            state.generation = generation;
        }
    }
    tracing::info!(
        event = "sync-ws",
        action = "audit_subscription_changed",
        caller_fp = context.fingerprint,
        socket_id = session.socket_id,
        domain = "audit",
        subscribed = subscribe,
        "the audit domain's subscription changed"
    );
    CommandOutcome::AuditSubscription { subscribed: subscribe }
}

fn layout_result(
    context: &ClientContext,
    result: &UiApplyLayoutResult,
) -> CommandOutcome {
    match (&context.tab_id, context.read_only, &context.viewer_key) {
        (Some(tab_id), false, Some(_)) => CommandOutcome::LayoutResult {
            tab_id: tab_id.clone(),
            result: Box::new(result.clone()),
        },
        // A read-only socket, or a socket with no tab, has no layout to settle.
        // Silently dropping it is correct: there is nothing to attribute the
        // result to, and answering one would attribute it to somebody else.
        _ => CommandOutcome::Nothing,
    }
}
