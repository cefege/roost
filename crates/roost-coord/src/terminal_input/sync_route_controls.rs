//! A Sync socket's input-route claim and transport probe: admitted only for a
//! writable, tab-bound socket and only against sessions and workers in its
//! live scope, reserved before any lookup, then correlated by the route owner.
//! A refusal is answered at once; a control that reached the worker and then
//! failed is not answered, because no truthful answer exists.
//! Ports the claim/probe half of `apps/coord/src/terminal/input/sync-terminal-controls.ts`.

use std::sync::Arc;

use roost_proto::{TerminalInputRouteClaim, TerminalTransportProbe};
use roost_protocol::wire::WorkerFp;

use crate::services::CoordServices;
use crate::sync_ws::control_frames::{
    input_route_refusal_frame, input_route_result_frame, transport_probe_refusal_frame,
    transport_probe_result_frame,
};
use crate::sync_ws::driver::SyncLink;
use crate::terminal_input::control_lane::resolve_session_route;
use crate::terminal_input::route_contract::{
    InputRouteClaimRequest, MAX_TERMINAL_INPUT_ROUTE_REVISION, RouteControlError,
    TransportProbeRequest, is_terminal_route_identifier,
};
use crate::terminal_input::route_sender::{
    TERMINAL_ROUTE_CONTROL_TIMEOUT_MS, is_current_terminal_input_route_worker,
};
use crate::terminal_input::sync_controls::{SyncControlSocket, reply, scope_has_session};
use crate::workers::hop_deadline::HopDeadline;

const ROUTE_UNAVAILABLE: &str = "terminal input route is unavailable";
const SESSION_UNAVAILABLE: &str = "terminal session is unavailable";

/// Admit one route claim, then carry it to the worker off the socket task.
pub(super) fn accept_route_claim(
    services: &Arc<CoordServices>,
    link: &Arc<SyncLink>,
    socket: SyncControlSocket,
    command: TerminalInputRouteClaim,
) {
    let refuse = |reason: &str| {
        let frame = input_route_refusal_frame(
            &command.request_id,
            &command.session_id,
            command.revision,
            &command.worker_epoch,
            reason,
        );
        reply(link, frame);
    };
    let shape_valid = [
        &command.request_id,
        &command.session_id,
        &command.worker_epoch,
    ]
    .into_iter()
    .all(|value| is_terminal_route_identifier(value))
        && (1..=MAX_TERMINAL_INPUT_ROUTE_REVISION).contains(&command.revision);
    if let Some(reason) = route_control_refusal(&socket) {
        return refuse(reason);
    }
    if !shape_valid {
        return refuse(ROUTE_UNAVAILABLE);
    }
    if !scope_has_session(link, &command.session_id) {
        return refuse(SESSION_UNAVAILABLE);
    }
    let results = Arc::clone(services.terminal_input.route_results());
    let slot = match results.reserve_control(&socket.socket_id, &command.request_id) {
        Ok(slot) => slot,
        Err(refusal) => return refuse(refusal.as_str()),
    };
    let services = Arc::clone(services);
    let link = Arc::clone(link);
    tokio::spawn(async move {
        let route =
            resolve_session_route(&services.db, &services.byte_hub, &command.session_id).await;
        let refusal = match &route {
            Err(_) => Some(ROUTE_UNAVAILABLE),
            Ok(Some(route))
                if scope_has_session(&link, &command.session_id)
                    && scope_has_worker(&link, &route.worker_fp) =>
            {
                None
            }
            Ok(_) => Some(SESSION_UNAVAILABLE),
        };
        let worker =
            match (refusal, route) {
                (None, Ok(Some(route))) => services
                    .workers
                    .current_routable(&route.worker_fp)
                    .filter(|worker| {
                        is_current_terminal_input_route_worker(
                            &services.workers,
                            worker,
                            &command.worker_epoch,
                        )
                    }),
                _ => None,
            };
        let Some(worker) = worker else {
            results.release_control(slot);
            let frame = input_route_refusal_frame(
                &command.request_id,
                &command.session_id,
                command.revision,
                &command.worker_epoch,
                refusal.unwrap_or(ROUTE_UNAVAILABLE),
            );
            return reply(&link, frame);
        };
        let request = InputRouteClaimRequest {
            browser_request_id: command.request_id.clone(),
            session_id: command.session_id.clone(),
            revision: command.revision,
            device_fingerprint: socket.device_fingerprint.clone(),
            tab_id: socket.tab_id.clone().unwrap_or_default(),
            connection_id: socket.socket_id.clone(),
            worker: Arc::clone(&worker),
            worker_epoch: command.worker_epoch.clone(),
            deadline: HopDeadline::start(TERMINAL_ROUTE_CONTROL_TIMEOUT_MS),
        };
        match results.claim(request, Some(slot)).await {
            Ok(result) => {
                let still_scoped = scope_has_session(&link, &command.session_id)
                    && scope_has_worker(&link, &worker.worker_fp)
                    && is_current_terminal_input_route_worker(
                        &services.workers,
                        &worker,
                        &command.worker_epoch,
                    );
                if still_scoped {
                    reply(&link, input_route_result_frame(result));
                }
            }
            Err(RouteControlError::Refused(refusal)) => {
                let frame = input_route_refusal_frame(
                    &command.request_id,
                    &command.session_id,
                    command.revision,
                    &command.worker_epoch,
                    refusal.as_str(),
                );
                reply(&link, frame);
            }
            Err(RouteControlError::Failed(error)) => {
                tracing::info!(session_id = %command.session_id, error = ?error.message,
                    "a terminal input route claim failed after it reached the worker");
            }
        }
    });
}

/// Admit one transport probe, then carry it to the worker off the socket task.
pub(super) fn accept_transport_probe(
    services: &Arc<CoordServices>,
    link: &Arc<SyncLink>,
    socket: SyncControlSocket,
    command: TerminalTransportProbe,
) {
    let refuse = || {
        reply(
            link,
            transport_probe_refusal_frame(&command.request_id, &command.worker_fp),
        )
    };
    let shape_valid = is_terminal_route_identifier(&command.request_id)
        && is_terminal_route_identifier(&command.worker_fp);
    let Ok(worker_fp) = WorkerFp::try_from(command.worker_fp.as_str()) else {
        return refuse();
    };
    if route_control_refusal(&socket).is_some()
        || !shape_valid
        || !scope_has_worker(link, &worker_fp)
    {
        return refuse();
    }
    let worker = services.workers.current_routable(&worker_fp);
    let Some((worker, worker_epoch)) = worker.and_then(|worker| {
        let epoch = worker.process_epoch.clone()?;
        is_current_terminal_input_route_worker(&services.workers, &worker, &epoch)
            .then_some((worker, epoch))
    }) else {
        return refuse();
    };
    let results = Arc::clone(services.terminal_input.route_results());
    let Ok(slot) = results.reserve_control(&socket.socket_id, &command.request_id) else {
        return refuse();
    };
    let request = TransportProbeRequest {
        browser_request_id: command.request_id.clone(),
        connection_id: socket.socket_id.clone(),
        worker_fp: command.worker_fp.clone(),
        worker: Arc::clone(&worker),
        worker_epoch: worker_epoch.clone(),
        deadline: HopDeadline::start(TERMINAL_ROUTE_CONTROL_TIMEOUT_MS),
    };
    let services = Arc::clone(services);
    let link = Arc::clone(link);
    tokio::spawn(async move {
        match results.probe(request, Some(slot)).await {
            Ok(result) => {
                let still_scoped = scope_has_worker(&link, &worker_fp)
                    && is_current_terminal_input_route_worker(
                        &services.workers,
                        &worker,
                        &worker_epoch,
                    );
                if still_scoped {
                    reply(&link, transport_probe_result_frame(result));
                }
            }
            Err(RouteControlError::Refused(_)) => {
                reply(
                    &link,
                    transport_probe_refusal_frame(&command.request_id, &command.worker_fp),
                );
            }
            Err(RouteControlError::Failed(error)) => {
                tracing::info!(worker_fp = %worker_fp, error = ?error.message,
                    "a terminal transport probe failed after it reached the worker");
            }
        }
    });
}

/// Why this socket may not issue route controls at all, or `None`.
fn route_control_refusal(socket: &SyncControlSocket) -> Option<&'static str> {
    if socket.read_only {
        return Some("this Sync socket cannot write terminal input");
    }
    if socket.viewer_key.is_none() || socket.tab_id.is_none() {
        return Some("terminal input route requires a tab-bound Sync socket");
    }
    None
}

fn scope_has_worker(link: &SyncLink, worker_fp: &WorkerFp) -> bool {
    link.lock().index.worker_fps.contains(worker_fp)
}
