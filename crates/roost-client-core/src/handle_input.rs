//! The input path: admission, the route a batch goes out on, truthful results,
//! and the retirement that decides `rejected` versus `ambiguous`.
//!
//! Split from `handle_terminal` because input is the one path where getting it
//! wrong writes to somebody's shell twice. The rule is short enough to state and
//! easy to break by accident: once a batch has been handed to a transport, its
//! fate is whatever the transport says, and nothing here re-sends a batch the
//! worker may have written. The one refusal the worker proves it never wrote
//! (`terminal input route changed`, zero bytes) is put back unsent and waits
//! for the Sync claim.
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
use crate::terminal::input::{INPUT_ROUTE_CHANGED_REASON, InputOutcome, PendingInput};
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
    let reclaiming = reclaim_lost_route(store, session_id, now_ms);
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
    // A promotion or a Sync fallback is asking a worker for the route epoch.
    // The batch waits unsent for that answer (v2 `admit` in `holding` or
    // `claiming`): sent now it would carry no epoch, and a worker that has
    // already moved the route refuses it as `terminal input route changed`.
    if store.input.is_holding(session_id) {
        tracing::debug!(
            target: "terminal",
            session_id,
            input_seq = admitted.input_seq,
            "terminal input held while the route is claimed"
        );
        if reclaiming {
            crate::handle_sync::claim_due_fallbacks(store, now_ms, out);
        }
        return;
    }
    // The destination is the session's ELECTED ROUTE, and failing that the Sync
    // socket this tab is dialled on — with NO pane required.
    //
    // A replica only exists once a pane has opened a view over the session, so
    // demanding one here refused every keystroke aimed at a session nobody was
    // looking at. v2 has no such gate: `terminalInputDestinationForSession`
    // takes the active direct route if there is one and otherwise the current
    // Sync v2 terminal state, which is the socket's, and resolves the worker's
    // epoch from the sessions projection beside it. Same two answers, same order.
    let Some(token) = input_destination(store, session_id) else {
        settle_as_unsent(
            store,
            session_id,
            admitted.input_seq,
            "no terminal transport is connected for this session",
        );
        return;
    };
    dispatch_startable(store, session_id, &token, now_ms, out);
    let queued = store
        .input
        .outstanding(session_id)
        .iter()
        .any(|pending| pending.input_seq == admitted.input_seq && !pending.started);
    if queued {
        tracing::debug!(
            target: "terminal",
            session_id,
            input_seq = admitted.input_seq,
            "terminal input queued behind the in-flight window"
        );
    }
}

/// Where a session's input goes now: its elected route's generation, else the
/// Sync socket this tab is dialled on.
pub(crate) fn input_destination(store: &Store, session_id: &str) -> Option<TerminalToken> {
    store
        .terminal(session_id)
        .and_then(|replica| replica.generation().cloned())
        .or_else(|| store.sync_terminal_token())
}

/// Hand `token`'s transport every unsent batch the session's in-flight window
/// has room for, oldest first, so keystrokes reach the PTY in the order typed.
pub(crate) fn dispatch_startable(
    store: &mut Store,
    session_id: &str,
    token: &TerminalToken,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    for pending in store.input.startable(session_id) {
        dispatch_batch(store, &pending, token, now_ms, out);
    }
}

/// A lane BLOCKED because its input route was lost claims Sync back on the
/// keystroke that finds it so, and that keystroke waits in the claim's hold
/// rather than being refused.
///
/// Nothing else reclaims it. A blocked lane refuses every batch, and with no
/// direct route left to promote, the worker goes on refusing epoch-less Sync
/// input as `terminal input route changed`: a pane that reads Coordinator and
/// takes no keystrokes. A session an elected direct route serves is that
/// route's to claim.
fn reclaim_lost_route(store: &mut Store, session_id: &str, now_ms: u64) -> bool {
    if !store.input.is_blocked(session_id) || store.routes.route(session_id).is_some() {
        return false;
    }
    let worker_fp = store
        .terminal(session_id)
        .map(|replica| replica.worker_fp.clone())
        .filter(|worker_fp| !worker_fp.is_empty())
        .or_else(|| {
            crate::store::selectors::session_by_id(store, session_id)
                .map(|session| session.worker_fp.as_str().to_owned())
        });
    let Some(worker_fp) = worker_fp else {
        return false;
    };
    let worker_epoch = store.input.acknowledged_worker_epoch(session_id).to_owned();
    store
        .input
        .begin_fallback(session_id, &worker_fp, &worker_epoch, now_ms);
    store.note_change();
    tracing::info!(
        target: "route",
        session_id,
        worker_fp,
        "a keystroke into a blocked input lane claims the Sync route back"
    );
    true
}

/// Hand one admitted batch to `token`'s transport, stamped with the route epoch
/// acknowledged over that connection. Used for a fresh batch and for one a
/// claim held, so both go out the same way.
fn dispatch_batch(
    store: &mut Store,
    admitted: &PendingInput,
    token: &TerminalToken,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let session_id = admitted.session_id.as_str();
    let wire_view_id = admitted.view_id.as_deref().and_then(|view_id| {
        store
            .terminal(session_id)
            .and_then(|replica| replica.wire_view_id(view_id))
            .map(str::to_string)
    });
    let input_route_epoch = store.input.route_epoch_for(session_id, token);
    store.input.mark_started(admitted.input_seq, token, now_ms);
    if token.transport == TerminalTransport::Sync {
        out.push(Effect::SendSync(SyncCommand::TerminalInput {
            session_id: session_id.to_string(),
            view_id: wire_view_id,
            input_seq: admitted.input_seq,
            bytes: admitted.bytes.clone(),
            input_route_epoch,
            token: token.clone(),
        }));
    } else {
        out.push(Effect::SendDirect {
            token: token.clone(),
            command: DirectCommand::Input {
                session_id: session_id.to_string(),
                view_id: wire_view_id,
                input_seq: admitted.input_seq,
                bytes: admitted.bytes.clone(),
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
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let carried_on_sync = store
        .input
        .outstanding(session_id)
        .iter()
        .find(|pending| pending.input_seq == input_seq)
        .and_then(|pending| pending.fence.as_ref())
        .is_some_and(|fence| fence.token.transport == TerminalTransport::Sync);
    // A refusal of a Sync batch while no direct route serves the session means
    // the worker gave the route to a connection this document no longer uses —
    // or never did, as after a reload in the same tab — and it will refuse
    // every epoch-less batch after it too. The worker refuses that way only
    // before writing, so the batch waits for the Sync claim instead of being
    // lost.
    let moved = carried_on_sync
        && store.routes.route(session_id).is_none()
        && matches!(
            outcome,
            InputOutcome::Rejected { reason, .. } if reason == INPUT_ROUTE_CHANGED_REASON
        );
    if moved && store.input.requeue_moved_sync_batch(session_id, input_seq) {
        store.note_change();
        if store.input.is_holding(session_id) || reclaim_lost_route(store, session_id, now_ms) {
            crate::handle_sync::claim_due_fallbacks(store, now_ms, out);
            tracing::warn!(
                target: "route",
                session_id,
                input_seq,
                "another connection held this session's input route; the batch waits for the Sync claim"
            );
            return;
        }
        tracing::warn!(
            target: "route",
            session_id,
            input_seq,
            "another connection holds this session's input route and no worker is known to claim it from"
        );
    }
    if !store.input.settle(input_seq, outcome.clone()) {
        return;
    }
    store.note_change();
    tracing::info!(
        target: "terminal",
        session_id,
        input_seq,
        status = outcome.status_name(),
        "terminal input settled"
    );
    send_queued_input(store, session_id, now_ms, out);
    // A promotion waits for the old route's batches to settle before it claims;
    // this may have been the last of them, and the sweep is up to a tick away.
    let drain_pending = store
        .input
        .claim_state(session_id)
        .and_then(|claims| claims.promotion.as_ref())
        .is_some_and(|promotion| !promotion.claim_sent);
    if drain_pending {
        crate::handle_sync::resume_held_promotion(store, session_id, now_ms, out);
    }
}

/// A settled batch freed a slot in the session's in-flight window: send what
/// waits behind it. Every path that settles a started batch calls this.
pub(crate) fn send_queued_input(
    store: &mut Store,
    session_id: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    if !store.input.is_holding(session_id)
        && let Some(token) = input_destination(store, session_id)
    {
        dispatch_startable(store, session_id, &token, now_ms, out);
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

    match token.transport {
        // A direct route's ids belong to the worker it died on, and the
        // coordinator has never heard of them. What the session needs first is a
        // set of ids the coordinator WILL accept, so the painted rows stay up and
        // the heartbeat stops publishing a dead handle; the baseline that follows
        // the fresh view acceptance is what repairs the stream.
        //
        // Its input route is the worker's too, and that worker still names the
        // dead carrier: Sync input without an epoch is refused until Sync claims
        // the route back. The lane HOLDS while it does (v2
        // `TerminalPeerFallbackClaims`), so a keystroke typed into the gap waits
        // for the claim instead of being refused; the hold has its own admission
        // timeout, so a claim that never lands cannot hold a batch forever.
        TerminalTransport::Loopback | TerminalTransport::Peer => {
            if let Some(worker_fp) = token.worker_fp.as_deref() {
                store
                    .input
                    .begin_fallback(session_id, worker_fp, &token.process_epoch, 0);
            }
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
