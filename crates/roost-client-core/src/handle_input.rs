//! The input path: admission, the route a batch goes out on, truthful results,
//! and the retirement that decides `rejected` versus `ambiguous`.
//!
//! Split from `handle_terminal` because input is the one path where getting it
//! wrong writes to somebody's shell twice. The rule is short enough to state and
//! easy to break by accident: once a batch has been handed to a transport, its
//! fate is whatever the transport says, and NOTHING here ever re-sends it.
//!
//! The view a batch names is the PANE, and the id it puts on the wire is the one
//! that pane's authority holds. After a promotion those differ, and a keystroke
//! stamped with the pane's own id would be written against a handle no worker is
//! watching — so the resolution happens here, once, on the way out.
//!
//! Ported from `apps/web/src/store/transport/terminal-input-router.ts`. Incident:
//! `docs/FAILURE-INDEX.md:1503`.

use crate::effect::{DirectCommand, Effect, SyncCommand};
use crate::store::Store;
use crate::terminal::InputPhase;
use crate::terminal::input::InputOutcome;
use crate::terminal::token::{TerminalToken, TerminalTransport};

/// Keystrokes from a pane. The core decides the route; the host only writes.
pub fn handle_terminal_input(
    store: &mut Store,
    session_id: &str,
    view_id: Option<&str>,
    bytes: &[u8],
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let admission = store.input.admit(
        session_id,
        view_id.map(str::to_string),
        bytes.to_vec(),
        now_ms,
    );
    store.input.outcome_feed.note_admission(view_id, &admission);
    if let Some(observer) = &mut store.input.smoke_observer {
        observer.observe_admission(session_id, &admission);
    }
    let admitted = match admission {
        Ok(admitted) => admitted,
        Err(refusal) => {
            tracing::info!(
                target: "terminal",
                session_id,
                reason = %refusal.reason,
                "terminal input refused at admission"
            );
            return;
        }
    };
    store.note_change();
    // The destination is the session's ELECTED ROUTE, and failing that the Sync
    // socket this tab is dialled on — with NO pane required.
    //
    // A replica only exists once a pane has opened a view over the session, so
    // demanding one here refused every keystroke aimed at a session nobody was
    // looking at. v2 has no such gate: `terminalInputDestinationForSession`
    // takes the active direct route if there is one and otherwise the current
    // Sync v2 terminal state, which is the socket's, and resolves the worker's
    // epoch from the sessions projection beside it. Same two answers, same order.
    let Some(token) = store
        .terminal(session_id)
        .and_then(|replica| replica.generation().cloned())
        .or_else(|| store.sync_terminal_token())
    else {
        settle_as_unsent(
            store,
            session_id,
            admitted.input_seq,
            "no terminal transport is connected for this session",
        );
        return;
    };
    let wire_view_id = view_id.and_then(|view_id| {
        store
            .terminal(session_id)
            .and_then(|replica| replica.wire_view_id(view_id))
            .map(str::to_string)
    });
    let input_route_epoch = store.input.route_epoch_for(session_id, &token);
    store.input.mark_started(admitted.input_seq, &token);
    if token.transport == TerminalTransport::Sync {
        out.push(Effect::SendSync(SyncCommand::TerminalInput {
            session_id: session_id.to_string(),
            view_id: wire_view_id,
            input_seq: admitted.input_seq,
            bytes: admitted.bytes,
            input_route_epoch,
            token,
        }));
    } else {
        out.push(Effect::SendDirect {
            token,
            command: DirectCommand::Input {
                session_id: session_id.to_string(),
                view_id: wire_view_id,
                input_seq: admitted.input_seq,
                bytes: admitted.bytes,
                input_route_epoch,
            },
        });
    }
}

/// A truthful write result for one admitted batch.
///
/// Applied from EITHER carrier, and correlated by the batch's own sequence rather
/// than by which route it went out on: a result that arrives after a promotion is
/// still the truth about that batch, and dropping it would leave the batch
/// outstanding forever.
pub fn handle_input_result(
    store: &mut Store,
    session_id: &str,
    input_seq: u64,
    outcome: &InputOutcome,
) {
    if store.input.settle(input_seq, outcome.clone()) {
        store.note_change();
        tracing::info!(
            target: "terminal",
            session_id,
            input_seq,
            status = outcome.status_name(),
            "terminal input settled"
        );
    }
}

/// Settle a batch that was admitted but will never be sent.
///
/// `rejected`, never `ambiguous`: nothing left this client, so there is nothing
/// in doubt and the reader can safely type it again.
pub fn settle_as_unsent(store: &mut Store, session_id: &str, input_seq: u64, reason: &str) {
    let _ = store.input.settle(
        input_seq,
        InputOutcome::Rejected {
            input_seq,
            reason: reason.to_string(),
        },
    );
    store.note_change();
    tracing::info!(
        target: "terminal",
        session_id,
        input_seq,
        reason,
        "terminal input settled unsent"
    );
}

/// Retire one route: settle what that carrier had already been sent, hold what it
/// had not, and repair from Sync. Nothing is replayed, and the painted rows stay.
pub fn retire_route(
    store: &mut Store,
    session_id: &str,
    token: &TerminalToken,
    reason: &str,
    out: &mut Vec<Effect>,
) {
    store.routes.retire_route(session_id, token);
    store.note_change();

    // `retire_token` settles only what THAT carrier had been sent. A batch on
    // another route is that route's business and is left alone — retiring one
    // route must not settle a batch another route is still carrying.
    for outcome in store.input.retire_token(token, reason) {
        tracing::warn!(
            target: "terminal",
            session_id,
            input_seq = outcome.input_seq(),
            status = outcome.status_name(),
            "terminal input settled by route loss"
        );
    }

    // The lane HOLDS rather than refuses. A new route may arrive before the reader
    // types again, and refusing would lose the keystroke for a gap nobody can see.
    // The hold has its own admission timeout, so a route that never comes back
    // cannot hold a batch forever.
    for outcome in store.input.set_phase(session_id, InputPhase::Holding) {
        tracing::info!(
            target: "terminal",
            session_id,
            input_seq = outcome.input_seq(),
            "unsent terminal input refused on route loss"
        );
    }

    match token.transport {
        // A direct route's ids belong to the worker it died on, and the
        // coordinator has never heard of them. What the session needs first is a
        // set of ids the coordinator WILL accept, so the painted rows stay up and
        // the heartbeat stops publishing a dead handle; the baseline that follows
        // the fresh view acceptance is what repairs the stream.
        TerminalTransport::Loopback | TerminalTransport::Peer => {
            crate::handle_terminal::begin_sync_view_rotation(store, session_id, token, out);
        }
        // A Sync socket that redialled keeps its ids — the coordinator is the same
        // authority — so the pane asks for a fresh baseline immediately rather
        // than waiting for a heartbeat to re-establish the generation.
        TerminalTransport::Sync => {
            if let Some(replica) = store.terminal(session_id) {
                let view_id = replica.repair_view().map(|view| view.wire_view_id.clone());
                if let (Some(view_id), Some(position), Some(sync_token)) = (
                    view_id,
                    replica.resync_position(),
                    store.sync_terminal_token(),
                ) {
                    out.push(Effect::SendSync(SyncCommand::TerminalResync {
                        session_id: session_id.to_string(),
                        view_id,
                        stream_id: position.stream_id,
                        grid_epoch: position.grid_epoch,
                        seq: position.seq,
                        token: sync_token,
                    }));
                }
            }
        }
    }
}
