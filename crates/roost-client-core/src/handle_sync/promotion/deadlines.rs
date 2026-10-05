//! The ends of a route claim that no answer settles: its deadline, the Sync
//! socket it was sent on closing, a promotion whose candidate went, and the
//! Sync fallback's retry clock.
//!
//! Called by the sweep (`handle_sync::sweep_route_claims`), by `handle_event`
//! when a Sync link closes, and by `handle_input` for the keystroke that just
//! began a Sync reclaim (`claim_due_fallbacks`). Ported from v2
//! `terminal-input-route-claim.ts` `awaitRouteClaim` and `sync-outbound.ts`
//! `rejectSyncClaims`.

use crate::client::local::outbound::CLAIM_SYNC_CLOSED;
use crate::effect::Effect;
use crate::store::Store;
use crate::sync::SyncDomain;
use crate::terminal::input::router::RouteClaim;

use super::{
    abandon_promotion, advance_promotion, block_lane, defer_or_block_fallback, send_claim,
};

/// The claims sent on a Sync socket that just closed end now, not at their
/// deadline: each is settled as an unanswered claim (v2 `rejectSyncClaims`
/// "terminal Sync closed", which the claim's caller sees as unconfirmed).
pub(crate) fn reject_sync_claims(
    store: &mut Store,
    socket_generation: u64,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    for claim in store.input.reject_sync_claims(socket_generation) {
        end_unanswered_claim(store, &claim, CLAIM_SYNC_CLOSED, now_ms, out);
    }
}

/// The claim deadlines: an unanswered claim, a promotion whose candidate is
/// gone or whose drain is still pending, and every Sync fallback due a retry.
pub(crate) fn sweep_route_claims(store: &mut Store, now_ms: u64, out: &mut Vec<Effect>) {
    for claim in store.input.expire_route_claims(now_ms) {
        let reason = "terminal input route claim was not confirmed";
        end_unanswered_claim(store, &claim, reason, now_ms, out);
    }
    for (session_id, _) in store.input.held_promotions() {
        resume_held_promotion(store, &session_id, now_ms, out);
    }
    claim_due_fallbacks(store, now_ms, out);
}

/// One held promotion, re-entered: claimed once its drain is over, abandoned
/// when its candidate went. Run by the sweep, and at once by the input result
/// that settles the old route's last batch, so the claim does not wait for the
/// next sweep as well.
pub(crate) fn resume_held_promotion(
    store: &mut Store,
    session_id: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let Some(promotion) = store
        .input
        .claim_state(session_id)
        .and_then(|claims| claims.promotion.clone())
    else {
        return;
    };
    let staged = store
        .routes
        .candidate(session_id)
        .is_some_and(|candidate| candidate.attempt_id == promotion.attempt_id);
    if !staged {
        abandon_promotion(store, session_id, "candidate was cancelled", now_ms, out);
    } else if !promotion.claim_sent {
        advance_promotion(
            store,
            session_id,
            promotion.attempt_id,
            &promotion.token,
            now_ms,
            out,
        );
    }
}

/// Claim Sync back for every fallback due an attempt now. Run by the sweep, and
/// at once by a keystroke that just started one, so the first key after a lost
/// route waits one round trip rather than a sweep interval as well.
pub(crate) fn claim_due_fallbacks(store: &mut Store, now_ms: u64, out: &mut Vec<Effect>) {
    for (session_id, fallback) in store.input.due_fallbacks(now_ms) {
        // A ready Sync generation only (v2 `readySyncTerminalInputDestination`):
        // a claim on a socket still subscribing would be refused by a
        // coordinator that has not admitted this tab's terminal domain.
        let sync = store
            .sync
            .domain_is_ready(SyncDomain::Terminal)
            .then(|| store.sync_terminal_token())
            .flatten();
        let worker_epoch = store
            .direct
            .live_grant(&fallback.worker_fp, now_ms)
            .map(|grant| grant.worker_epoch.clone())
            .filter(|epoch| !epoch.is_empty())
            .unwrap_or(fallback.worker_epoch);
        let Some(sync) = sync.filter(|_| !worker_epoch.is_empty()) else {
            let reason = "terminal Sync fallback input route was unavailable";
            defer_or_block_fallback(store, &session_id, reason, now_ms);
            continue;
        };
        match store
            .input
            .begin_route_claim(&session_id, &sync, &worker_epoch, None, now_ms)
        {
            Ok(claim) => send_claim(&claim, out),
            Err(reason) => block_lane(store, &session_id, reason),
        }
    }
}

/// A claim whose answer will never be read: its promotion is abandoned, its
/// fallback tried again shortly.
fn end_unanswered_claim(
    store: &mut Store,
    claim: &RouteClaim,
    reason: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    tracing::warn!(
        target: "route",
        session_id = %claim.session_id,
        reason,
        "route claim ended unanswered"
    );
    match claim.attempt_id {
        Some(_) => abandon_promotion(store, &claim.session_id, reason, now_ms, out),
        None => defer_or_block_fallback(store, &claim.session_id, reason, now_ms),
    }
}
