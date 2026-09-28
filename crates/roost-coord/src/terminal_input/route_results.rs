//! The coordinator's typed correlation of input-route controls: a Sync socket
//! reserves a bounded slot per browser nonce, the claim or probe goes to one
//! exact worker generation, and only that generation's typed result settles it.
//! The browser nonce is restored only after every worker and result fence
//! passes. Built once on `TerminalInputRuntime`; the slot table is `route_slots`.
//! Ports `apps/coord/src/terminal/input/terminal-input-route-results.ts`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use connectrpc::{ConnectError, ErrorCode};
use roost_proto::{
    TerminalInputRouteResult, TerminalTransportProbeResult, WTerminalInputRouteResult,
    WTerminalTransportProbeResult,
};
use roost_protocol::wire::WorkerFp;

use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::terminal_input::route_contract::{
    InputRouteClaimRequest, MAX_TERMINAL_INPUT_ROUTE_REVISION, RouteControlError,
    RouteControlRefusal, TransportProbeRequest, is_terminal_route_identifier,
    is_valid_input_route_result, is_valid_transport_probe_result,
};
use crate::terminal_input::route_retirements::TerminalInputRouteRetirements;
use crate::terminal_input::route_sender::{
    InputRouteClaimSend, RouteCorrelation, is_current_terminal_input_route_worker,
    send_terminal_input_route_claim_request, send_terminal_transport_probe_request,
};
use crate::terminal_input::route_slots::RouteControlTable;
use crate::terminal_input::route_state::{
    PendingRouteControl, RouteControlKind, RouteControlSlotId,
};
use crate::terminal_screen::pending_rpcs::PendingRpcs;
use crate::terminal_screen::typed_results::TypedWorkerResult;

/// The typed-result owner shared by the Sync socket and the worker link.
#[derive(Debug)]
pub struct TerminalInputRouteResults {
    workers: Arc<WorkerRegistry>,
    pending_rpcs: Arc<PendingRpcs>,
    table: Mutex<RouteControlTable>,
    retirements: TerminalInputRouteRetirements,
}

/// Releases a slot however its control ends, including a dropped waiter.
struct SlotRelease<'a> {
    owner: &'a TerminalInputRouteResults,
    slot: RouteControlSlotId,
}

impl Drop for SlotRelease<'_> {
    fn drop(&mut self) {
        self.owner.release_control(self.slot);
    }
}

impl TerminalInputRouteResults {
    /// An owner over the process's worker registry and pending-request table.
    #[must_use]
    pub fn new(workers: Arc<WorkerRegistry>, pending_rpcs: Arc<PendingRpcs>) -> Self {
        Self {
            workers,
            pending_rpcs,
            table: Mutex::new(RouteControlTable::default()),
            retirements: TerminalInputRouteRetirements::new(),
        }
    }

    /// Send one route claim and wait for its validated typed result.
    pub async fn claim(
        &self,
        request: InputRouteClaimRequest,
        reserved: Option<RouteControlSlotId>,
    ) -> Result<TerminalInputRouteResult, RouteControlError> {
        let slot = self.table().require(
            &request.connection_id,
            &request.browser_request_id,
            reserved,
        )?;
        let _release = SlotRelease { owner: self, slot };
        let well_formed = [
            &request.browser_request_id,
            &request.session_id,
            &request.device_fingerprint,
            &request.tab_id,
            &request.connection_id,
            &request.worker_epoch,
        ]
        .into_iter()
        .all(|value| is_terminal_route_identifier(value));
        let worker = &request.worker;
        if !well_formed
            || !(1..=MAX_TERMINAL_INPUT_ROUTE_REVISION).contains(&request.revision)
            || !is_current_terminal_input_route_worker(&self.workers, worker, &request.worker_epoch)
        {
            return Err(RouteControlRefusal::InputRouteUnavailable.into());
        }
        let claim = Some((request.session_id.as_str(), request.revision));
        let mut expected = self.bind(
            slot,
            &request.connection_id,
            worker,
            &request.worker_epoch,
            RouteControlKind::Claim,
            claim,
        )?;
        let send = InputRouteClaimSend {
            session_id: request.session_id.clone(),
            device_fingerprint: request.device_fingerprint.clone(),
            tab_id: request.tab_id.clone(),
            browser_connection_id: request.connection_id.clone(),
            revision: request.revision,
            worker_epoch: request.worker_epoch.clone(),
        };
        let correlation = self.correlation(slot, expected.binding);
        let worker_request = send_terminal_input_route_claim_request(
            &self.workers,
            &self.pending_rpcs,
            worker,
            &send,
            correlation,
            request.deadline,
        );
        if !worker_request.is_admitted() {
            return Err(RouteControlRefusal::InputRouteUnavailable.into());
        }
        expected.outer_request_id = worker_request.request_id().map(str::to_owned);
        tracing::debug!(worker_fp = %worker.worker_fp, session_id = %request.session_id,
            revision = request.revision, "terminal input route claim sent");
        let reply = worker_request
            .result()
            .await
            .map_err(RouteControlError::Failed)?;
        if !self.is_current_pending_worker(&expected, worker) {
            return Err(failed("terminal input route worker changed before reply"));
        }
        let valid = is_valid_input_route_result(&expected, &reply);
        let Some(result) = reply.result.as_option().filter(|_| valid) else {
            return Err(failed(
                "terminal input route worker returned an invalid result",
            ));
        };
        Ok(TerminalInputRouteResult {
            request_id: request.browser_request_id,
            ..result.clone()
        })
    }

    /// Send one transport probe and wait for its validated typed result.
    pub async fn probe(
        &self,
        request: TransportProbeRequest,
        reserved: Option<RouteControlSlotId>,
    ) -> Result<TerminalTransportProbeResult, RouteControlError> {
        let slot = self.table().require(
            &request.connection_id,
            &request.browser_request_id,
            reserved,
        )?;
        let _release = SlotRelease { owner: self, slot };
        let well_formed = [
            &request.browser_request_id,
            &request.connection_id,
            &request.worker_fp,
            &request.worker_epoch,
        ]
        .into_iter()
        .all(|value| is_terminal_route_identifier(value));
        let worker = &request.worker;
        if !well_formed
            || request.worker_fp != worker.worker_fp.as_str()
            || !is_current_terminal_input_route_worker(&self.workers, worker, &request.worker_epoch)
        {
            return Err(RouteControlRefusal::TransportProbeUnavailable.into());
        }
        let mut expected = self.bind(
            slot,
            &request.connection_id,
            worker,
            &request.worker_epoch,
            RouteControlKind::Probe,
            None,
        )?;
        let correlation = self.correlation(slot, expected.binding);
        let worker_request = send_terminal_transport_probe_request(
            &self.workers,
            &self.pending_rpcs,
            worker,
            &request.worker_epoch,
            correlation,
            request.deadline,
        );
        if !worker_request.is_admitted() {
            return Err(RouteControlRefusal::TransportProbeUnavailable.into());
        }
        expected.outer_request_id = worker_request.request_id().map(str::to_owned);
        let reply = worker_request
            .result()
            .await
            .map_err(RouteControlError::Failed)?;
        if !self.is_current_pending_worker(&expected, worker) {
            return Err(failed(
                "terminal transport probe worker changed before reply",
            ));
        }
        if !is_valid_transport_probe_result(&expected, &reply) {
            return Err(failed(
                "terminal transport probe worker returned an invalid result",
            ));
        }
        Ok(TerminalTransportProbeResult {
            request_id: request.browser_request_id,
            worker_fp: request.worker_fp,
            worker_epoch: expected.worker_epoch,
            __buffa_unknown_fields: Default::default(),
        })
    }

    /// Reserve one bounded control slot before any asynchronous route lookup.
    pub fn reserve_control(
        &self,
        connection_id: &str,
        browser_request_id: &str,
    ) -> Result<RouteControlSlotId, RouteControlRefusal> {
        self.table().reserve(connection_id, browser_request_id)
    }

    /// Release an unused admission or a completed control.
    pub fn release_control(&self, slot: RouteControlSlotId) {
        self.table().release(slot);
    }

    /// Cancel a closing Sync socket's pending controls and retire every worker
    /// route it ever claimed.
    pub fn retire_browser_connection(&self, connection_id: &str) {
        let (released, workers) = self.table().release_connection(connection_id);
        let cancelled = self.cancel_pending(released);
        tracing::info!(
            connection_id,
            cancelled,
            workers = workers.len(),
            "terminal input routes retired for a closed Sync socket"
        );
        for (worker_fp, worker_epoch) in workers {
            self.retirements
                .retire(&self.workers, &worker_fp, &worker_epoch, connection_id);
        }
    }

    /// Flush the retirements retained while this worker was unroutable.
    pub fn flush_worker_retirements(&self, worker_fp: &WorkerFp) {
        self.retirements.flush(&self.workers, worker_fp);
    }

    /// Settle a claim from the exact generation it was sent to.
    pub fn accept_input_route_result(
        &self,
        source: &Arc<WorkerHandle>,
        frame: &WTerminalInputRouteResult,
    ) -> bool {
        self.settle(
            source,
            &frame.request_id,
            RouteControlKind::Claim,
            |pending| is_valid_input_route_result(pending, frame),
        )
        .is_some_and(|worker_fp| {
            let result = TypedWorkerResult::InputRoute(frame.clone());
            self.pending_rpcs
                .resolve_typed(result, Some(worker_fp.as_str()))
        })
    }

    /// Settle a probe from the exact generation it was sent to.
    pub fn accept_transport_probe_result(
        &self,
        source: &Arc<WorkerHandle>,
        frame: &WTerminalTransportProbeResult,
    ) -> bool {
        self.settle(
            source,
            &frame.request_id,
            RouteControlKind::Probe,
            |pending| is_valid_transport_probe_result(pending, frame),
        )
        .is_some_and(|worker_fp| {
            let result = TypedWorkerResult::TransportProbe(frame.clone());
            self.pending_rpcs
                .resolve_typed(result, Some(worker_fp.as_str()))
        })
    }

    /// Cancel only the controls captured against this exact worker generation.
    pub fn cancel_for_worker_handle(&self, worker: &Arc<WorkerHandle>, reason: &str) {
        let released = self.table().release_for_worker(worker);
        let cancelled = self.cancel_pending(released);
        if cancelled > 0 {
            tracing::info!(worker_fp = %worker.worker_fp, reason, cancelled,
                "terminal input route controls cancelled for a worker generation");
        }
    }

    /// Bind a control into its slot and remember the worker epoch it reaches.
    fn bind(
        &self,
        slot: RouteControlSlotId,
        connection_id: &str,
        worker: &Arc<WorkerHandle>,
        worker_epoch: &str,
        kind: RouteControlKind,
        claim: Option<(&str, u64)>,
    ) -> Result<PendingRouteControl, RouteControlRefusal> {
        let mut table = self.table();
        let pending = table.bind(slot, worker, worker_epoch, kind, claim)?;
        table.track_worker(connection_id, &worker.worker_fp, worker_epoch);
        Ok(pending)
    }

    /// Stop correlating the pending control an outer id names, when the result
    /// came from its generation, is of its kind, and answers it. Returns the
    /// worker its pending request is namespaced to.
    fn settle(
        &self,
        source: &Arc<WorkerHandle>,
        outer: &str,
        kind: RouteControlKind,
        answers: impl FnOnce(&PendingRouteControl) -> bool,
    ) -> Option<WorkerFp> {
        let mut table = self.table();
        let pending = table.pending_for_outer(outer)?;
        if pending.kind != kind
            || !self.is_current_pending_worker(pending, source)
            || !answers(pending)
        {
            return None;
        }
        let worker_fp = pending.worker_fp.clone();
        table.forget_outer(outer);
        Some(worker_fp)
    }

    /// The install/verify pair the sender calls around its write.
    fn correlation(
        &self,
        slot: RouteControlSlotId,
        binding: u64,
    ) -> RouteCorrelation<impl FnOnce(&str) -> Result<(), String> + '_, impl FnOnce() -> bool + '_>
    {
        RouteCorrelation {
            install: move |request_id: &str| self.table().install_outer(slot, binding, request_id),
            still_pending: move || self.table().is_bound(slot, binding),
        }
    }

    fn is_current_pending_worker(
        &self,
        pending: &PendingRouteControl,
        source: &Arc<WorkerHandle>,
    ) -> bool {
        Arc::ptr_eq(source, &pending.worker)
            && source.connection_generation == pending.connection_generation
            && source.worker_fp == pending.worker_fp
            && is_current_terminal_input_route_worker(&self.workers, source, &pending.worker_epoch)
    }

    /// Cancel each released control's worker request; returns how many had one.
    fn cancel_pending(&self, released: Vec<PendingRouteControl>) -> usize {
        let mut cancelled = 0;
        for pending in released {
            let Some(outer) = pending.outer_request_id else {
                continue;
            };
            self.pending_rpcs
                .cancel(&outer, Some(pending.worker_fp.as_str()));
            cancelled += 1;
        }
        cancelled
    }

    fn table(&self) -> MutexGuard<'_, RouteControlTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn failed(message: &str) -> RouteControlError {
    RouteControlError::Failed(ConnectError::new(ErrorCode::Unknown, message))
}
