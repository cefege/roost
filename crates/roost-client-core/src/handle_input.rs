//! The input path: admission, the route a batch goes out on, truthful results,
//! and the retirement that decides `rejected` versus `ambiguous`.
//!
//! Split from `handle_terminal` because input is the one path where getting it
//! wrong writes to somebody's shell twice. The rule is short enough to state and
//! easy to break by accident: once a batch has been handed to a transport, its
//! fate is whatever the transport says, and NOTHING here ever re-sends it.
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
    let admitted = match store.input.admit(
        session_id,
        view_id.map(str::to_string),
        bytes.to_vec(),
        now_ms,
    ) {
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
    // The route is the generation the session's replica is fenced to. That is the
    // generation whose grid the keystroke belongs to, so it is the one whose
    // authority has to accept the write.
    let Some(token) = store
        .terminal(session_id)
        .and_then(|replica| replica.generation().cloned())
    else {
        settle_as_unsent(
            store,
            session_id,
            admitted.input_seq,
            "terminal transport is not connected",
        );
        return;
    };
    let input_route_epoch = store.input.route_epoch_for(session_id, &token);
    store.input.mark_started(admitted.input_seq, &token);
    if token.transport == TerminalTransport::Sync {
        out.push(Effect::SendSync(SyncCommand::TerminalInput {
            session_id: session_id.to_string(),
            view_id: view_id.map(str::to_string),
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
                view_id: view_id.map(str::to_string),
                input_seq: admitted.input_seq,
                bytes: admitted.bytes,
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

    // The replica repairs from Sync when there is a Sync route at all. With
    // painted rows still up, a session with no carrier shows what it last had
    // rather than blanking.
    if let Some(replica) = store.terminal(session_id) {
        let view_id = replica.repair_view().map(|view| view.view_id.clone());
        if let Some(view_id) = view_id
            && let Some(sync_token) = store.sync_terminal_token()
        {
            out.push(Effect::SendSync(SyncCommand::TerminalResync {
                session_id: session_id.to_string(),
                view_id,
                token: sync_token,
            }));
        }
    }
}
