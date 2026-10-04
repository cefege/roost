//! The input-route half of a direct promotion and of a direct route's loss:
//! hold fresh input, drain what the old route carries, claim the worker's route
//! for the new one, then commit and release — or recover the route on Sync.
//!
//! Called by `candidate::fold_into_candidate` (a candidate earned its baseline),
//! by `apply_frame` and `handle_direct_frame` (a claim's answer) and by the
//! sweep (drain and answer deadlines, fallback retries). The lane state is
//! `terminal::input::router::claim`'s. Ported from
//! `apps/web/src/store/transport/terminal-peer-promotions.ts` and
//! `terminal-peer-fallback.ts`.

mod deadlines;

pub(crate) use self::deadlines::{claim_due_fallbacks, reject_sync_claims, sweep_route_claims};

use crate::effect::{DirectCommand, Effect, SyncCommand};
use crate::handle_input::dispatch_batch;
use crate::store::Store;
use crate::sync::inbound::InputRouteResult;
use crate::terminal::input::router::{ClaimSettlement, INPUT_HANDOFF_DRAIN_MS, RouteClaim};
use crate::terminal::token::{TerminalToken, TerminalTransport};

/// A candidate is promotable: hold the session's input, wait out the old
/// route's batches, and claim the worker's input route on the candidate.
///
/// Re-entered by every frame the candidate folds and by every sweep, so each
/// step is taken once: the hold when none names this attempt, the claim once
/// the drain is over, and nothing while the claim's answer is outstanding.
pub(super) fn advance_promotion(
    store: &mut Store,
    session_id: &str,
    attempt_id: u64,
    candidate: &TerminalToken,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    if let Err(refusal) = store.routes.promotable(session_id, attempt_id, candidate) {
        tracing::debug!(
            target: "route",
            session_id,
            ?refusal,
            "the candidate is not promotable, so the fallback keeps the terminal"
        );
        return;
    }
    let held = store
        .input
        .claim_state(session_id)
        .and_then(|claims| claims.promotion.clone())
        .filter(|promotion| promotion.attempt_id == attempt_id);
    let held_at_ms = match held {
        Some(promotion) if promotion.claim_sent => return,
        Some(promotion) => promotion.held_at_ms,
        None => {
            if !claims_input_route(store, candidate, now_ms) {
                if store
                    .input
                    .claim_state(session_id)
                    .is_some_and(|claims| claims.required)
                {
                    let reason = "terminal peer input-route capability is unavailable";
                    abandon_promotion(store, session_id, reason, now_ms, out);
                    return;
                }
                commit_candidate(store, session_id, attempt_id, candidate, now_ms, out);
                return;
            }
            store
                .input
                .hold_for_promotion(session_id, attempt_id, candidate, now_ms);
            store.note_change();
            tracing::info!(
                target: "route",
                session_id,
                attempt_id,
                transport = candidate.transport.as_str(),
                "a promotable candidate holds the session's input while the route is claimed"
            );
            now_ms
        }
    };
    // v2 `drain`: the old route's started batches settle before the worker is
    // told the route moved, or one of them could be refused as from a stale
    // route after it was already written.
    let old_route = store
        .terminal(session_id)
        .and_then(|replica| replica.generation().cloned());
    if let Some(old_route) = old_route
        && store.input.has_unsettled_on(session_id, &old_route)
    {
        if now_ms.saturating_sub(held_at_ms) >= INPUT_HANDOFF_DRAIN_MS {
            let reason = "terminal input route did not drain before the promotion deadline";
            abandon_promotion(store, session_id, reason, now_ms, out);
        }
        return;
    }
    match store.input.begin_route_claim(
        session_id,
        candidate,
        &candidate.process_epoch,
        Some(attempt_id),
        now_ms,
    ) {
        Ok(claim) => send_claim(&claim, out),
        Err(reason) => abandon_promotion(store, session_id, reason, now_ms, out),
    }
}

/// One claim's answer, arriving on `token`. A promotion's acceptance commits
/// the candidate and releases the held input onto it with the new epoch; a
/// fallback's releases the input onto Sync. A refusal abandons the promotion or
/// schedules the fallback's next attempt.
pub(crate) fn settle_route_result(
    store: &mut Store,
    token: &TerminalToken,
    result: &InputRouteResult,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let Some(settlement) = store.input.settle_route_claim(token, result, now_ms) else {
        tracing::debug!(
            target: "route",
            session_id = %result.session_id,
            request_id = %result.request_id,
            "a route-claim answer for no claim in flight"
        );
        return;
    };
    store.note_change();
    let session_id = result.session_id.as_str();
    match settlement {
        ClaimSettlement::Accepted(claim) => {
            tracing::info!(
                target: "route",
                session_id,
                revision = claim.revision,
                transport = claim.token.transport.as_str(),
                "the worker acknowledged the input route"
            );
            match claim.attempt_id {
                Some(attempt_id) => {
                    if store
                        .routes
                        .promotable(session_id, attempt_id, &claim.token)
                        .is_err()
                    {
                        let reason = "candidate promotion was superseded";
                        abandon_promotion(store, session_id, reason, now_ms, out);
                        return;
                    }
                    commit_candidate(store, session_id, attempt_id, &claim.token, now_ms, out);
                    release_held_onto(store, session_id, &claim.token, now_ms, out);
                }
                // Onto the token the answer arrived on, not the one the claim
                // was sent under: they are the same connection, and only the
                // arrival's terminal-domain generation is still current.
                None => release_held_onto(store, session_id, token, now_ms, out),
            }
        }
        ClaimSettlement::Retry(claim) => send_claim(&claim, out),
        ClaimSettlement::Refused { claim, reason } => {
            tracing::warn!(
                target: "route",
                session_id,
                revision = claim.revision,
                reason = %reason,
                "the worker refused the input-route claim"
            );
            match claim.attempt_id {
                Some(_) => abandon_promotion(store, session_id, &reason, now_ms, out),
                None => defer_or_block_fallback(store, session_id, &reason, now_ms),
            }
        }
    }
}

/// Whether input on `candidate` is written under a claimed route epoch (v2
/// `inputRouteSupported`). A peer always is: its worker refuses peer input
/// with no epoch. Loopback is when the worker's grant says it implements
/// `terminal-input-route-v1`.
fn claims_input_route(store: &Store, candidate: &TerminalToken, now_ms: u64) -> bool {
    let granted = candidate
        .worker_fp
        .as_deref()
        .and_then(|worker_fp| store.direct.live_grant(worker_fp, now_ms))
        .map(|grant| grant.input_route_supported);
    granted.unwrap_or(candidate.transport == TerminalTransport::Peer)
}

/// Send one claim on the route it names.
fn send_claim(claim: &RouteClaim, out: &mut Vec<Effect>) {
    tracing::info!(
        target: "route",
        session_id = %claim.session_id,
        revision = claim.revision,
        transport = claim.token.transport.as_str(),
        "input route claimed"
    );
    if claim.token.transport == TerminalTransport::Sync {
        out.push(Effect::SendSync(SyncCommand::TerminalInputRouteClaim {
            session_id: claim.session_id.clone(),
            request_id: claim.request_id.clone(),
            revision: claim.revision,
            worker_epoch: claim.worker_epoch.clone(),
            token: claim.token.clone(),
        }));
    } else {
        out.push(Effect::SendDirect {
            token: claim.token.clone(),
            command: DirectCommand::RouteClaim {
                session_id: claim.session_id.clone(),
                request_id: claim.request_id.clone(),
                revision: claim.revision,
                worker_epoch: claim.worker_epoch.clone(),
                domain_generation: claim.token.domain_generation,
            },
        });
    }
}

/// Make the candidate the session's route.
fn commit_candidate(
    store: &mut Store,
    session_id: &str,
    attempt_id: u64,
    candidate: &TerminalToken,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    match store.routes.promote(session_id, attempt_id, candidate) {
        Ok(promoted) => {
            super::candidate::commit_promotion(store, session_id, promoted, candidate, now_ms, out);
        }
        Err(refusal) => tracing::debug!(
            target: "route",
            session_id,
            ?refusal,
            "the candidate is not promotable, so the fallback keeps the terminal"
        ),
    }
}

/// End a promotion that will not commit (v2 `cancelPromotion`). Its candidate
/// goes; its held input is released onto the route still in charge — unless
/// the claim may already have moved the worker's route, in which case Sync has
/// to claim it back first (v2 `recoverOrRelease`).
fn abandon_promotion(
    store: &mut Store,
    session_id: &str,
    reason: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let promotion = store
        .input
        .claim_state(session_id)
        .and_then(|claims| claims.promotion.clone());
    tracing::warn!(target: "route", session_id, reason, "a direct promotion was abandoned");
    let Some(promotion) = promotion else {
        return;
    };
    if store
        .routes
        .candidate(session_id)
        .is_some_and(|candidate| candidate.attempt_id == promotion.attempt_id)
    {
        crate::handle_terminal::cancel_staged_candidate(store, session_id, reason, out);
    }
    if promotion.claim_sent
        && let Some(worker_fp) = promotion.token.worker_fp.as_deref()
    {
        store.input.begin_fallback(
            session_id,
            worker_fp,
            &promotion.token.process_epoch,
            now_ms,
        );
        return;
    }
    let current = store
        .terminal(session_id)
        .and_then(|replica| replica.generation().cloned())
        .or_else(|| store.sync_terminal_token());
    match current {
        Some(current) => release_held_onto(store, session_id, &current, now_ms, out),
        None => block_lane(store, session_id, "terminal transport is not connected"),
    }
}

/// End the hold and send what it kept on `route` — when the epoch the worker
/// requires for it is the one this lane holds (v2 `hold.release(destination)`,
/// then `release()` when the destination was refused).
fn release_held_onto(
    store: &mut Store,
    session_id: &str,
    route: &TerminalToken,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let required = store
        .input
        .claim_state(session_id)
        .is_some_and(|claims| claims.required);
    if required && store.input.route_epoch_for(session_id, route).is_empty() {
        block_lane(store, session_id, "terminal input route was not claimed");
        return;
    }
    let held = store.input.release_held(session_id);
    tracing::info!(
        target: "route",
        session_id,
        held = held.len(),
        transport = route.transport.as_str(),
        "held terminal input released onto the claimed route"
    );
    for pending in held {
        dispatch_batch(store, &pending, route, now_ms, out);
    }
}

/// A fallback attempt failed: try again shortly, or block once they are spent.
fn defer_or_block_fallback(store: &mut Store, session_id: &str, reason: &str, now_ms: u64) {
    if !store.input.defer_fallback(session_id, now_ms) {
        block_lane(store, session_id, reason);
    }
}

/// The route cannot be recovered: refuse what is held, and every batch after.
fn block_lane(store: &mut Store, session_id: &str, reason: &str) {
    let refused = store.input.block_route(session_id);
    store.note_change();
    tracing::warn!(
        target: "route",
        session_id,
        reason,
        refused = refused.len(),
        "terminal input blocked: the route could not be claimed"
    );
}
