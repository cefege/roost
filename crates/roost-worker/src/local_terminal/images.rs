//! Direct image reads from an authenticated terminal port.
//! Reads use the session core's retained content-key store and are re-authorized
//! immediately before the response is sent on the history lane.

use std::sync::Arc;

use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_proto::{LocalImageRequest, LocalImageResponse};
use roost_protocol::terminal_peer::peer::TerminalPeerPacketLane;

use super::authority::{Carrier, PortSession};
use super::port::PacketSendResult;
use super::sockets::LocalTerminalSockets;

/// What a peer image read reserves on the history lane: the largest PNG the
/// core serves (`roost_term` caps it at 2 MiB) plus room for the envelope.
const IMAGE_RESERVATION_BYTES: usize = 2 * 1024 * 1024 + 4 * 1024;

impl LocalTerminalSockets {
    /// Fetch one PNG only while the live grant authorizes the named session.
    pub(super) fn start_image(&self, session: &Arc<PortSession>, request: LocalImageRequest) {
        let png = if self.is_session_authorized(session, &request.session_id) {
            let session_id =
                roost_protocol::wire::brand::SessionId::try_from(request.session_id.as_str());
            session_id.ok().and_then(|session_id| {
                self.authorization
                    .sessions
                    .with_record_mut(&session_id, |record| {
                        record
                            .terminal_core
                            .image_png(request.image_key)
                            .map(|bytes| bytes.to_vec())
                    })
                    .flatten()
            })
        } else {
            None
        };
        let peer = match &session.carrier {
            Carrier::Peer { port, .. } => Some(Arc::clone(port)),
            Carrier::Loopback(_) => None,
        };
        let reservation = peer
            .as_ref()
            .and_then(|port| port.reserve_history_read(IMAGE_RESERVATION_BYTES));
        let reservation_failed = peer.is_some() && reservation.is_none();
        let sockets = self.self_handle.clone();
        let session = Arc::clone(session);
        self.runtime.spawn(async move {
            let Some(owner) = sockets.upgrade() else {
                return;
            };
            let response = if !owner.is_session_authorized(&session, &request.session_id) {
                image_error(&request, "terminal session is unavailable")
            } else if reservation_failed {
                image_error(
                    &request,
                    "terminal image response exceeds direct transport limit",
                )
            } else if let Some(png) = png {
                LocalImageResponse {
                    request_id: request.request_id.clone(),
                    session_id: request.session_id.clone(),
                    image_key: request.image_key,
                    png,
                    ..Default::default()
                }
            } else {
                image_error(&request, "terminal image not found")
            };
            let mut reservation = reservation;
            if let Some(reservation) = reservation.as_mut() {
                reservation.transfer();
            }
            let sent = owner.send_frame(
                &session,
                ServerFrame::from(response),
                TerminalPeerPacketLane::History,
                false,
            );
            if sent != PacketSendResult::Refused
                && let Some(peer) = peer.as_ref()
            {
                peer.wait_for_lane_drain(TerminalPeerPacketLane::History)
                    .await;
            }
        });
    }
}

fn image_error(request: &LocalImageRequest, error: &str) -> LocalImageResponse {
    LocalImageResponse {
        request_id: request.request_id.clone(),
        session_id: request.session_id.clone(),
        image_key: request.image_key,
        error: error.to_owned(),
        ..Default::default()
    }
}
