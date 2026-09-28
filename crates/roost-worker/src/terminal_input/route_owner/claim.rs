//! Route claiming for the terminal input route owner: validation, the
//! exact-latest dedup, admission against the work budget and the keeper lane,
//! and the attempt `route_attempt::complete_claim` finishes on its own task.
//! Ports the `claim` half of `apps/worker/src/terminal/terminal-input-route-owner.ts`.
//! Called by `terminal_input::port` (coordinator link) and the local door.

use std::sync::Arc;

use roost_proto::TerminalInputRouteResult;
use roost_protocol::wire::brand::ChannelId;
use tokio::sync::watch;

use super::{
    MAX_ROUTE_REVISION, PendingAttempt, RouteActor, RouteClaim, RouteClaimBudget, RouteEntry,
    RouteStatus, TERMINAL_INPUT_ROUTE_MAX_ENTRIES, TerminalInputRouteOwner, pre_admission_failure,
    prune_retired, route_key, valid_actor, valid_identifier,
};
use crate::session::keeper_admission::{Admission, AdmissionKind};
use crate::terminal_input::route_attempt::{
    AttemptCancel, AttemptRun, complete_claim, route_result,
};
use crate::terminal_input::work_budget::ROUTE_CLAIM_BUSY;
use crate::uplink::OwnerFuture;

impl TerminalInputRouteOwner {
    /// Claim an actor/session input route. Everything up to keeper admission
    /// happens in this call, so the prior epoch stops being current before the
    /// returned future is first polled; the lane wait runs on its own task.
    /// Must be called inside the worker's tokio runtime.
    pub fn claim(
        &self,
        actor: RouteActor,
        claim: RouteClaim,
        budget: Box<dyn RouteClaimBudget>,
    ) -> OwnerFuture<TerminalInputRouteResult> {
        let shared = &self.shared;
        let refuse = |latest_revision: u64,
                      reason: &str|
         -> OwnerFuture<TerminalInputRouteResult> {
            tracing::info!(session_id = %claim.session_id, revision = claim.revision, reason, "terminal input route claim refused");
            let result = route_result(
                &claim,
                &shared.worker_epoch,
                false,
                latest_revision,
                "",
                reason,
            );
            Box::pin(std::future::ready(result))
        };
        let mut state = shared.lock();
        prune_retired(&mut state, (shared.now)());
        let identifiers = [&claim.request_id, &claim.session_id, &claim.worker_epoch];
        if !valid_actor(&actor)
            || !identifiers.iter().all(|value| valid_identifier(value))
            || claim.revision == 0
            || claim.revision > MAX_ROUTE_REVISION
        {
            return refuse(0, "invalid_route_claim");
        }
        if state.disposed {
            return refuse(0, ROUTE_CLAIM_BUSY);
        }
        if state.revoke_overflow || state.revoked_devices.contains(&actor.device_fingerprint) {
            return refuse(0, "device_revoked");
        }
        if claim.worker_epoch != shared.worker_epoch {
            return refuse(0, "worker_epoch_mismatch");
        }
        let key = route_key(&actor, &claim.session_id);
        let existing = state.routes.get(&key);
        if let Some(existing) = existing {
            let exact_latest = existing.latest_claim.request_id == claim.request_id
                && existing.latest_claim.revision == claim.revision
                && existing.actor.connection_id == actor.connection_id;
            if exact_latest && let Some(pending) = &existing.pending {
                return awaited(pending.outcome.clone(), &claim, &shared.worker_epoch);
            }
            if exact_latest && let Some(latest) = &existing.latest_result {
                return Box::pin(std::future::ready(latest.clone()));
            }
            if claim.revision <= existing.latest_revision {
                return refuse(existing.latest_revision, "stale_route_revision");
            }
        }
        let latest_revision = existing.map_or(0, |entry| entry.latest_revision);
        let channel_id = shared.channel_of(&claim.session_id);
        if let Some(failure) = pre_admission_failure(channel_id, budget.as_ref()) {
            return refuse(latest_revision, failure);
        }
        if existing.is_none() && state.routes.len() >= TERMINAL_INPUT_ROUTE_MAX_ENTRIES {
            return refuse(0, ROUTE_CLAIM_BUSY);
        }
        if existing.is_some_and(|entry| entry.pending.is_some()) {
            return refuse(latest_revision, ROUTE_CLAIM_BUSY);
        }
        let actor_session_key = format!(
            "{:?}",
            (
                &actor.device_fingerprint,
                &actor.tab_id,
                &actor.connection_id,
                &claim.session_id
            )
        );
        let reservation = match shared
            .work_budget
            .reserve_route_claim(&actor.connection_id, &actor_session_key)
        {
            Ok(reservation) => reservation,
            Err(reason) => return refuse(latest_revision, reason),
        };
        let (Some(channel_id), Some(branded)) = (channel_id, channel_id.and_then(brand)) else {
            return refuse(latest_revision, "terminal session is unavailable");
        };
        let ticket = match shared.lanes.admit(branded, AdmissionKind::TerminalInput) {
            Admission::Granted(ticket) => ticket,
            Admission::Refused(reason) => return refuse(latest_revision, reason),
        };
        state.next_attempt += 1;
        let attempt_id = state.next_attempt;
        let cancel = Arc::new(AttemptCancel::default());
        let (publish, outcome) = watch::channel(None);
        let entry = state
            .routes
            .entry(key.clone())
            .or_insert_with(|| RouteEntry {
                actor: actor.clone(),
                session_id: claim.session_id.clone(),
                latest_revision: 0,
                input_route_epoch: None,
                status: RouteStatus::Retired,
                retired_until: None,
                latest_claim: claim.clone(),
                latest_result: None,
                pending: None,
            });
        entry.actor = actor.clone();
        entry.session_id = claim.session_id.clone();
        entry.latest_revision = claim.revision;
        entry.input_route_epoch = None;
        entry.status = RouteStatus::Blocked;
        entry.retired_until = None;
        entry.latest_claim = claim.clone();
        entry.latest_result = None;
        entry.pending = Some(PendingAttempt {
            id: attempt_id,
            connection_id: actor.connection_id.clone(),
            cancel: Arc::clone(&cancel),
            outcome: outcome.clone(),
        });
        drop(state);
        tracing::info!(session_id = %claim.session_id, revision = claim.revision, "terminal input route claim is waiting for the keeper lane");
        let waiting = awaited(outcome, &claim, &shared.worker_epoch);
        tokio::spawn(complete_claim(AttemptRun {
            shared: Arc::clone(shared),
            key,
            attempt_id,
            claim,
            channel_id,
            connection_id: actor.connection_id,
            ticket,
            reservation,
            cancel,
            budget,
            publish,
        }));
        waiting
    }
}

/// Wait for an attempt's published result. A publisher that vanished without
/// one is answered as busy, as v2 answers a claim whose promise rejected.
fn awaited(
    mut outcome: watch::Receiver<Option<TerminalInputRouteResult>>,
    claim: &RouteClaim,
    worker_epoch: &str,
) -> OwnerFuture<TerminalInputRouteResult> {
    let busy = route_result(claim, worker_epoch, false, 0, "", ROUTE_CLAIM_BUSY);
    Box::pin(async move {
        loop {
            if let Some(result) = outcome.borrow_and_update().clone() {
                return result;
            }
            if outcome.changed().await.is_err() {
                return outcome.borrow().clone().unwrap_or(busy);
            }
        }
    })
}

fn brand(channel_id: u16) -> Option<ChannelId> {
    ChannelId::try_from(i64::from(channel_id)).ok()
}
