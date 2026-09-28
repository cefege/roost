//! One client frame off a Sync socket: decoded strictly, fenced by the
//! negotiation, applied to the link, and its outcome carried out.
//!
//! Called by `sync_ws::socket` for every binary frame; the decision about what
//! a v2 frame MEANS is `sync_ws::commands`, and this module is what acts on
//! it. Ports `apps/coord/src/sync/sync-ws-client-ingress.ts` and the reply
//! half of `sync-ws-v2-commands.ts` (domain resets, refusals, the audit
//! toggle's reset notice).
//!
//! DECODED WITH NO ROOM FOR UNKNOWN FIELDS. v2 decodes with
//! `readUnknownFields: false` and then refuses any frame whose canonical
//! re-encoding differs from its bytes, so a frame carrying a field this build
//! does not know closes `1008`. `buffa` keeps unknown fields and would
//! re-encode them, which would let such a frame pass the canonical check; an
//! unknown-field budget of zero makes the decode itself refuse it, which is
//! the same close for the same frame.
//!
//! EVERY REFUSAL NAMES ITS CHECK. The close is `1008` for all of them, so the
//! log line is the only place a cause can live: without the check that refused
//! the frame, the acknowledgement and last sequence the socket held, and the
//! frame's own bytes, one reported cause stood for five of them.

use std::collections::BTreeSet;

use roost_proto::buffa::DecodeOptions;
use roost_proto::{SyncClientFrame, SyncDomain};

use crate::sync_ws::commands::{
    CommandOutcome, TerminalCommand, handle_client_frame, is_canonical_client_frame,
};
use crate::sync_ws::control_frames::ResetNotice;
use crate::sync_ws::driver::{Delivery, LinkClose, LinkState, SyncLink};
use crate::sync_ws::feed::FeedRuntime;
use crate::sync_ws::invalid_frame::{InvalidFrame, frame_hex};
use roost_proto::__buffa::oneof::sync_client_frame::Command;

/// What the socket task must do beyond the link: the parts of an outcome that
/// own a bus subscription or reach another runtime, and so must run with the
/// link unlocked.
#[derive(Debug)]
pub enum IngressEffect {
    /// Nothing outside the link.
    Nothing,
    /// Start (`true`) or stop the install-wide audit source.
    AuditSubscription(bool),
    /// Settle a layout acknowledgement against the UI runtime.
    LayoutResult(CommandOutcome),
    /// Replay a domain's retained state after its `domain_ready`, narrowed to
    /// the admitted sessions for the terminal domain (`sync-ws-v2-commands.ts:133`).
    SeedDomain {
        /// The domain that became ready.
        domain: SyncDomain,
        /// The sessions the snapshot token admitted, for the terminal domain.
        admitted_sessions: Option<BTreeSet<String>>,
    },
    /// Carry out a terminal command that passed the gate: view commands reach
    /// the view hub, which answers through this socket's own sink.
    Terminal(TerminalCommand),
}

/// Apply one binary client frame to `link`.
pub fn accept_client_frame(
    link: &SyncLink,
    feed: &FeedRuntime,
    raw: &[u8],
    now_ms: u64,
) -> IngressEffect {
    let mut guard = link.lock();
    let state = &mut *guard;
    // A socket that never negotiated the window reads nothing from its client,
    // and a closing one reads nothing more (`sync-ws-client-ingress.ts:34`).
    let flow_control = match &state.delivery {
        Delivery::V1(v1) => v1.flow_control(),
        Delivery::V2(_) => true,
    };
    if state.close.is_some() || !flow_control {
        return IngressEffect::Nothing;
    }
    let decoded = DecodeOptions::new()
        .with_unknown_field_limit(0)
        .decode_from_slice::<SyncClientFrame>(raw);
    let frame = match decoded {
        Ok(frame) if is_canonical_client_frame(&frame, raw) => frame,
        _ => {
            log_refused_frame(state, InvalidFrame::NotCanonical, None, raw, now_ms);
            return IngressEffect::Nothing;
        }
    };
    let outcome = match &mut state.delivery {
        Delivery::V1(v1) => {
            if v1.accept_ack(&frame, now_ms).is_err() {
                state.decide_close(LinkClose::INVALID_ACK, "invalid_ack", "client", now_ms);
            }
            return IngressEffect::Nothing;
        }
        Delivery::V2(session) => {
            // The command gate reads the socket's scope; v2 reads the LIVE set
            // (`sync-ws-v2-commands.ts:124-126`), so it is refreshed for the
            // one command that reads it rather than frozen at upgrade.
            if matches!(frame.command, Some(Command::DomainReady(_))) {
                state
                    .context
                    .session_ids
                    .clone_from(&state.index.session_ids);
            }
            let context = &state.context;
            feed.with_snapshot_tokens(|tokens| {
                handle_client_frame(session, context, &frame, tokens, now_ms)
            })
        }
    };
    apply_outcome(state, outcome, &frame, raw, now_ms)
}

/// A text frame on a Sync socket: the client contract is binary only, so a
/// socket that reads its client closes `1008`, and one that does not (no
/// `flow=1`) ignores it like every other client frame
/// (`sync-ws-client-ingress.ts:34-38`).
pub fn refuse_text_frame(link: &SyncLink, now_ms: u64) {
    let mut state = link.lock();
    let reads_client = match &state.delivery {
        Delivery::V1(v1) => v1.flow_control(),
        Delivery::V2(_) => true,
    };
    if reads_client {
        state.decide_close(
            LinkClose::INVALID_ACK,
            "text_client_frame",
            "client",
            now_ms,
        );
    }
}

/// Carry out one v2 outcome on the link.
fn apply_outcome(
    state: &mut LinkState,
    outcome: CommandOutcome,
    frame: &SyncClientFrame,
    raw: &[u8],
    now_ms: u64,
) -> IngressEffect {
    match outcome {
        CommandOutcome::Nothing => IngressEffect::Nothing,
        CommandOutcome::Acknowledged { released } => {
            tracing::debug!(event = "sync-ws", action = "ack", socket_id = %state.socket_id, released);
            IngressEffect::Nothing
        }
        CommandOutcome::DomainReady {
            domain,
            admitted_sessions,
        } => {
            // `handle_domain_ready` raised the flush request; the socket task
            // runs the flush turn that reads it before it waits again, which
            // is what lets this domain's held frames flow.
            tracing::info!(
                event = "sync-ws",
                action = "domain_ready",
                socket_id = %state.socket_id,
                domain = ?domain,
                admitted_sessions = admitted_sessions.as_ref().map_or(0, |set| set.len()),
                "a Sync domain closed its snapshot/live gap"
            );
            IngressEffect::SeedDomain {
                domain,
                admitted_sessions,
            }
        }
        CommandOutcome::AuditSubscription { subscribed } => {
            if !subscribed {
                announce_unsubscribed(state, now_ms);
            }
            IngressEffect::AuditSubscription(subscribed)
        }
        outcome @ CommandOutcome::LayoutResult { .. } => IngressEffect::LayoutResult(outcome),
        CommandOutcome::Terminal(command) => IngressEffect::Terminal(command),
        CommandOutcome::Refusal(frame) => {
            state.send_control(&frame, now_ms);
            IngressEffect::Nothing
        }
        CommandOutcome::ResetTerminal(notice) => {
            tracing::info!(
                event = "sync-ws",
                action = "domain_reset",
                socket_id = %state.socket_id,
                domain = "terminal",
                generation = notice.generation,
                reason = notice.reason,
                "the terminal domain was reset by its snapshot fence"
            );
            state.send_control(&notice.to_frame(), now_ms);
            IngressEffect::Nothing
        }
        CommandOutcome::Invalid(refused) => {
            log_refused_frame(state, refused, Some(frame), raw, now_ms);
            IngressEffect::Nothing
        }
    }
}

/// Report a refused client frame and close `1008`, with the evidence that tells
/// the five causes apart: which check refused it, what the client
/// acknowledged against what this socket has sent, and the frame's own bytes.
fn log_refused_frame(
    state: &mut LinkState,
    refused: InvalidFrame,
    frame: Option<&SyncClientFrame>,
    raw: &[u8],
    now_ms: u64,
) {
    let Delivery::V2(session) = &state.delivery else {
        state.decide_close(LinkClose::INVALID_ACK, refused.cause(), "client", now_ms);
        return;
    };
    let last_sent_seq = session.next_delivery_seq().saturating_sub(1);
    let acknowledged_seq = session.acknowledged_sequence();
    tracing::warn!(
        event = "sync-ws",
        action = "client_frame_refused",
        socket_id = %state.socket_id,
        path = refused.cause(),
        client_socket_id = frame.map_or("", |frame| frame.socket_id.as_str()),
        ack_delivery_seq = frame.and_then(|frame| frame.ack_delivery_seq),
        last_sent_seq,
        acknowledged_seq,
        frame_bytes = raw.len(),
        frame_hex = %frame_hex(raw),
        "a Sync client frame was not a legal client frame"
    );
    state.decide_close(LinkClose::INVALID_ACK, refused.cause(), "client", now_ms);
}

/// The reset an unsubscribe owes the client: the audit domain's new
/// generation, unsubscribed (`sync-ws-v2-commands.ts:165-175`).
fn announce_unsubscribed(state: &mut LinkState, now_ms: u64) {
    let Delivery::V2(session) = &state.delivery else {
        return;
    };
    let Some(generation) = session.domain_generation(SyncDomain::Audit) else {
        return;
    };
    let notice = ResetNotice {
        domain: SyncDomain::Audit,
        generation,
        reason: "unsubscribed",
        subscribed: false,
        terminal_sessions_dropped: false,
    };
    state.send_control(&notice.to_frame(), now_ms);
}
