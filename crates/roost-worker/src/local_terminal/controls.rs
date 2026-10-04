//! Authenticated direct-port controls outside the carrier lifecycle: input
//! route claims, transport probes and history reads. They receive only live
//! predicates and the owner's send paths, never keep a grant scope across a
//! call, and answer on the control or history lane rather than as cell
//! traffic. Called by `super::sockets::on_message`. Ports
//! `apps/worker/src/local-door/local-terminal-socket-controls.ts`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};

use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_proto::{
    LocalScrollbackRequest, LocalScrollbackResponse, TerminalInputRouteClaim,
    TerminalTransportProbe, TerminalTransportProbeResult,
};
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_WORKER_APPLICATION_QUEUE_MAX_BYTES, TerminalPeerLaneByteCaps,
    TerminalPeerPacketLane,
};

use super::authority::{
    Carrier, DirectClaimBudget, DirectPortBudget, PortSession, direct_port_actor,
};
use super::port::{HistoryReadReservation, PacketSendResult, PeerTerminalPacketPort};
use super::scrollback::read_local_scrollback;
use super::sockets::LocalTerminalSockets;
use crate::terminal_input::RouteClaim;

const HISTORY_PENDING: &str = "scrollback request is already pending";
const HISTORY_TOO_LARGE: &str = "scrollback response exceeds direct transport limit";
/// What a peer history read reserves: the history lane's logical cap less
/// room for the envelope.
const PEER_HISTORY_RESERVATION_BYTES: usize = TerminalPeerLaneByteCaps::HISTORY - 4 * 1024;

#[derive(Debug, Default)]
struct ControlsState {
    /// Ports with a history read in flight; one at a time per port.
    history_reads: HashSet<String>,
    /// History bytes reserved by loopback reads, against the worker-wide
    /// application queue a peer lane would have charged.
    loopback_history_reserved: usize,
}

/// The per-port history bookkeeping. v2 `LocalTerminalPortControls` state.
#[derive(Debug, Default)]
pub(super) struct PortControls {
    state: Mutex<ControlsState>,
}

impl PortControls {
    pub(super) fn retire_port(&self, socket_id: &str) {
        self.lock().history_reads.remove(socket_id);
    }

    fn lock(&self) -> MutexGuard<'_, ControlsState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// What a started history read holds until it finishes.
struct HistoryRead {
    peer: Option<Arc<dyn PeerTerminalPacketPort>>,
    reservation: Option<Box<dyn HistoryReadReservation>>,
    loopback_reserved: bool,
}

impl LocalTerminalSockets {
    /// Claim an input route for the port's actor; the claim's admission runs
    /// now, in receive order, and only its answer waits.
    pub(super) fn start_claim(&self, session: &Arc<PortSession>, command: TerminalInputRouteClaim) {
        let Some(actor) = direct_port_actor(session) else {
            return;
        };
        let budget = DirectClaimBudget {
            budget: DirectPortBudget::new(&self.authorization, session),
            session_id: command.session_id.clone(),
        };
        let claim = RouteClaim {
            request_id: command.request_id,
            session_id: command.session_id,
            revision: command.revision,
            worker_epoch: command.worker_epoch,
        };
        let pending = self
            .authorization
            .routes
            .claim(actor, claim, Box::new(budget));
        let sockets = self.self_handle.clone();
        let session = Arc::clone(session);
        self.runtime.spawn(async move {
            let result = pending.await;
            if let Some(sockets) = sockets.upgrade() {
                sockets.send_control(&session, ServerFrame::from(result));
            }
        });
    }

    /// Answer a probe addressed to this worker; any other is not ours.
    pub(super) fn probe(&self, session: &Arc<PortSession>, request: &TerminalTransportProbe) {
        if request.worker_fp != self.worker_fingerprint {
            return;
        }
        let result = TerminalTransportProbeResult {
            request_id: request.request_id.clone(),
            worker_fp: self.worker_fingerprint.clone(),
            worker_epoch: self.authorization.worker_epoch.clone(),
            ..Default::default()
        };
        self.send_control(session, ServerFrame::from(result));
    }

    /// One history read per port, reserved before the read starts.
    pub(super) fn start_scrollback(
        &self,
        session: &Arc<PortSession>,
        request: LocalScrollbackRequest,
    ) {
        let socket_id = session.socket_id().to_owned();
        if self.controls.lock().history_reads.contains(&socket_id) {
            self.send_history(session, history_error(&request, HISTORY_PENDING), true);
            return;
        }
        let peer = match &session.carrier {
            Carrier::Peer { port, .. } => Some(Arc::clone(port)),
            Carrier::Loopback(_) => None,
        };
        let reservation = peer
            .as_ref()
            .and_then(|port| port.reserve_history_read(PEER_HISTORY_RESERVATION_BYTES));
        if peer.is_some() && reservation.is_none() {
            self.send_history(session, history_error(&request, HISTORY_TOO_LARGE), true);
            return;
        }
        let loopback_reserved = peer.is_none();
        {
            let mut state = self.controls.lock();
            if loopback_reserved {
                let bytes = TerminalPeerLaneByteCaps::HISTORY;
                if state.loopback_history_reserved
                    > TERMINAL_PEER_WORKER_APPLICATION_QUEUE_MAX_BYTES - bytes
                {
                    drop(state);
                    self.send_history(session, history_error(&request, HISTORY_TOO_LARGE), true);
                    return;
                }
                state.loopback_history_reserved += bytes;
            }
            state.history_reads.insert(socket_id);
        }
        let read = HistoryRead {
            peer,
            reservation,
            loopback_reserved,
        };
        let sockets = self.self_handle.clone();
        let session = Arc::clone(session);
        self.runtime.spawn(async move {
            let Some(owner) = sockets.upgrade() else {
                return;
            };
            owner.finish_scrollback(&session, &request, read).await;
        });
    }

    async fn finish_scrollback(
        &self,
        session: &Arc<PortSession>,
        request: &LocalScrollbackRequest,
        mut read: HistoryRead,
    ) {
        let allows = |session_id: &str| self.is_session_authorized(session, session_id);
        let response = read_local_scrollback(
            &self.manager,
            &self.authorization.sessions,
            request,
            &allows,
        )
        .await;
        self.deliver_scrollback(session, request, response, &mut read)
            .await;
        drop(read.reservation.take());
        let mut state = self.controls.lock();
        if read.loopback_reserved {
            state.loopback_history_reserved -= TerminalPeerLaneByteCaps::HISTORY;
        }
        state.history_reads.remove(session.socket_id());
    }

    /// Re-authorize, send, and wait for the peer's history lane to drain.
    async fn deliver_scrollback(
        &self,
        session: &Arc<PortSession>,
        request: &LocalScrollbackRequest,
        mut response: LocalScrollbackResponse,
        read: &mut HistoryRead,
    ) {
        if !self.is_session_authorized(session, &request.session_id) {
            response = history_error(request, "terminal session is unavailable");
        }
        if let Some(reservation) = read.reservation.as_mut() {
            reservation.transfer();
        }
        let failed = !response.error.is_empty();
        let delivered = self.send_history(session, response, false);
        if delivered != PacketSendResult::Refused {
            self.drain_history(read.peer.as_ref()).await;
        } else if failed {
            self.close(session, "local history delivery refused");
        } else if self.send_history(session, history_error(request, HISTORY_TOO_LARGE), true)
            != PacketSendResult::Refused
        {
            self.drain_history(read.peer.as_ref()).await;
        }
    }

    async fn drain_history(&self, peer: Option<&Arc<dyn PeerTerminalPacketPort>>) {
        if let Some(port) = peer {
            port.wait_for_lane_drain(TerminalPeerPacketLane::History)
                .await;
        }
    }

    fn send_history(
        &self,
        session: &Arc<PortSession>,
        response: LocalScrollbackResponse,
        close_on_refusal: bool,
    ) -> PacketSendResult {
        self.send_frame(
            session,
            ServerFrame::from(response),
            TerminalPeerPacketLane::History,
            close_on_refusal,
        )
    }
}

fn history_error(request: &LocalScrollbackRequest, error: &str) -> LocalScrollbackResponse {
    LocalScrollbackResponse {
        request_id: request.request_id.clone(),
        error: error.to_owned(),
        ..Default::default()
    }
}
