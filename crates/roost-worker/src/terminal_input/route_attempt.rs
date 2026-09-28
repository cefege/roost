//! The keeper-lane half of one input-route claim: wait for the write-ordering
//! lane (or a cancellation), recheck live authority, and either activate the
//! route under a fresh epoch or retire it. Spawned by
//! `route_owner::TerminalInputRouteOwner::claim`. Ports `completeClaim`,
//! `failAttempt` and the attempt cancellation of
//! `apps/worker/src/terminal/terminal-input-route-owner.ts`.

use std::sync::{Arc, Mutex};

use roost_proto::TerminalInputRouteResult;
use tokio::sync::{Notify, watch};

use super::route_owner::{
    RouteClaim, RouteClaimBudget, RouteKey, RouteShared, RouteStatus,
    TERMINAL_INPUT_ROUTE_TOMBSTONE, pre_admission_failure, prune_retired,
};
use super::work_budget::RouteClaimReservation;
use crate::session::ids::mint_uuid;
use crate::session::keeper_admission::AdmissionTicket;

/// A pending claim's cancellation: the first reason wins.
#[derive(Debug, Default)]
pub(super) struct AttemptCancel {
    reason: Mutex<Option<&'static str>>,
    wake: Notify,
}

impl AttemptCancel {
    pub(super) fn request(&self, reason: &'static str) {
        let mut held = self
            .reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if held.is_none() {
            *held = Some(reason);
            self.wake.notify_one();
        }
    }

    fn reason(&self) -> Option<&'static str> {
        *self
            .reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Everything one attempt's task owns.
pub(super) struct AttemptRun {
    pub(super) shared: Arc<RouteShared>,
    pub(super) key: RouteKey,
    pub(super) attempt_id: u64,
    pub(super) claim: RouteClaim,
    pub(super) channel_id: u16,
    pub(super) connection_id: String,
    pub(super) ticket: AdmissionTicket,
    pub(super) reservation: RouteClaimReservation,
    pub(super) cancel: Arc<AttemptCancel>,
    pub(super) budget: Box<dyn RouteClaimBudget>,
    pub(super) publish: watch::Sender<Option<TerminalInputRouteResult>>,
}

/// Run one attempt to its result. A cancelled attempt answers at once but
/// keeps its claim capacity charged until its lane ticket actually drains:
/// a ticket cannot leave the lane early, and releasing the capacity first
/// would let retired claims pile unbounded tickets behind a held lane.
pub(super) async fn complete_claim(run: AttemptRun) {
    let AttemptRun {
        shared,
        key,
        attempt_id,
        claim,
        channel_id,
        connection_id,
        ticket,
        reservation,
        cancel,
        budget,
        publish,
    } = run;
    let granted = ticket.granted();
    tokio::pin!(granted);
    let cancelled = tokio::select! {
        biased;
        () = cancel.wake.notified() => true,
        () = &mut granted => false,
    };
    if cancelled {
        let reason = cancel.reason().unwrap_or("route_claim_retired");
        let result = route_result(
            &claim,
            &shared.worker_epoch,
            false,
            claim.revision,
            "",
            reason,
        );
        publish.send_replace(Some(result));
        granted.await;
        ticket.release();
        drop(reservation);
        clear_drained_pending(&shared, &key, attempt_id);
        return;
    }
    let result = settle_granted(
        &shared,
        &key,
        attempt_id,
        &claim,
        channel_id,
        &connection_id,
        &cancel,
        budget.as_ref(),
    );
    ticket.release();
    drop(reservation);
    publish.send_replace(Some(result));
}

/// The lane is granted: activate the route if this attempt is still the one
/// the entry is waiting on and live authority survived the wait.
#[allow(clippy::too_many_arguments)]
fn settle_granted(
    shared: &RouteShared,
    key: &RouteKey,
    attempt_id: u64,
    claim: &RouteClaim,
    channel_id: u16,
    connection_id: &str,
    cancel: &AttemptCancel,
    budget: &dyn RouteClaimBudget,
) -> TerminalInputRouteResult {
    let still_here = shared.channel_of(&claim.session_id) == Some(channel_id);
    let now = (shared.now)();
    let mut state = shared.lock();
    let disposed = state.disposed;
    let Some(entry) = state.routes.get_mut(key) else {
        return route_result(
            claim,
            &shared.worker_epoch,
            false,
            claim.revision,
            "",
            cancel.reason().unwrap_or("route_claim_retired"),
        );
    };
    let current = !disposed
        && entry
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == attempt_id)
        && entry.status == RouteStatus::Blocked
        && entry.latest_revision == claim.revision
        && entry.actor.connection_id == connection_id;
    if !current {
        let reason = cancel.reason().unwrap_or("route_claim_retired");
        return route_result(
            claim,
            &shared.worker_epoch,
            false,
            entry.latest_revision,
            "",
            reason,
        );
    }
    let failure = pre_admission_failure(still_here.then_some(channel_id), budget);
    let epoch = match (failure, mint_uuid()) {
        (None, Ok(epoch)) => epoch,
        (Some(reason), _) => return fail_attempt(shared, entry, claim, now, reason),
        (None, Err(error)) => {
            tracing::error!(%error, "terminal input route epoch could not be minted");
            return fail_attempt(shared, entry, claim, now, "route epoch could not be minted");
        }
    };
    let result = route_result(
        claim,
        &shared.worker_epoch,
        true,
        entry.latest_revision,
        &epoch,
        "",
    );
    entry.input_route_epoch = Some(epoch);
    entry.status = RouteStatus::Active;
    entry.retired_until = None;
    entry.pending = None;
    entry.latest_result = Some(result.clone());
    tracing::info!(session_id = %claim.session_id, revision = claim.revision, "terminal_input_route_active");
    result
}

fn fail_attempt(
    shared: &RouteShared,
    entry: &mut super::route_owner::RouteEntry,
    claim: &RouteClaim,
    now: std::time::Instant,
    reason: &str,
) -> TerminalInputRouteResult {
    let result = route_result(
        claim,
        &shared.worker_epoch,
        false,
        entry.latest_revision,
        "",
        reason,
    );
    entry.input_route_epoch = None;
    entry.status = RouteStatus::Retired;
    entry.retired_until = Some(now + TERMINAL_INPUT_ROUTE_TOMBSTONE);
    entry.pending = None;
    entry.latest_result = Some(result.clone());
    tracing::warn!(session_id = %claim.session_id, reason, "terminal_input_route_retired");
    result
}

/// A retired attempt's ticket has drained: the entry stops waiting on it.
fn clear_drained_pending(shared: &RouteShared, key: &RouteKey, attempt_id: u64) {
    let now = (shared.now)();
    let mut state = shared.lock();
    if let Some(entry) = state.routes.get_mut(key)
        && entry.status == RouteStatus::Retired
        && entry
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == attempt_id)
    {
        entry.pending = None;
        prune_retired(&mut state, now);
    }
}

/// The one result shape every claim answers with.
pub(super) fn route_result(
    claim: &RouteClaim,
    worker_epoch: &str,
    accepted: bool,
    latest_revision: u64,
    input_route_epoch: &str,
    reason: &str,
) -> TerminalInputRouteResult {
    TerminalInputRouteResult {
        request_id: claim.request_id.clone(),
        session_id: claim.session_id.clone(),
        revision: claim.revision,
        accepted,
        latest_revision,
        input_route_epoch: input_route_epoch.to_owned(),
        worker_epoch: worker_epoch.to_owned(),
        reason: reason.to_owned(),
        ..TerminalInputRouteResult::default()
    }
}
