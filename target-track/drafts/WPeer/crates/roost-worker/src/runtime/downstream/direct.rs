//! The direct-terminal downstream arms: a peer offer answered through the
//! fenced uplink once negotiated, a peer cancel, a transport probe answered in
//! receive order, and a direct retire. Called by `runtime::downstream`'s
//! dispatch; routes to [`DirectTerminalPort`] (`peer::DirectTerminal`). Ports
//! the terminal-peer, probe and retire cases of v2
//! `apps/worker/src/transport/coord-link-direct-terminal.ts`.

use std::sync::Arc;
use std::time::Instant;

use roost_proto::{
    DLocalTerminalPeerCancel, DLocalTerminalPeerOffer, DTerminalDirectRetire, DTerminalTransportProbe,
};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use super::owner_task::run_owner;
use super::replies::{self, PeerErrorKey};
use super::{Dispatcher, DownstreamLink};
use crate::link_ports::DirectTerminalPort;
use crate::peer::TerminalPeerOfferFailure;
use crate::uplink::RequestBudget;

impl Dispatcher {
    fn direct(&self) -> Option<Arc<dyn DirectTerminalPort>> {
        self.owners.as_ref().and_then(|owners| owners.direct.clone())
    }

    /// v2 `localTerminalPeerOffer`: no owner answers `disabled` now; an owner's
    /// answer or refusal goes out only on the connection the offer came in on.
    pub(super) fn terminal_peer_offer(
        &self,
        request: DLocalTerminalPeerOffer,
        received: Instant,
        link: &mut dyn DownstreamLink,
    ) {
        let key = PeerErrorKey::from(&request);
        let Some(direct) = self.direct() else {
            link.reply(replies::terminal_peer_error(&key, &self.process_epoch, TerminalPeerOfferFailure::Disabled.as_str()));
            return;
        };
        let budget = RequestBudget::from_budget_ms(request.budget_ms, received);
        let fence = self.uplink.fence();
        let reply_fence = fence.clone();
        let uplink = self.uplink.clone();
        let epoch = self.process_epoch.clone();
        run_owner(
            move || direct.peer_offer(request, budget, fence),
            Box::new(move |outcome| {
                let frame = match outcome {
                    Ok(Ok(answer)) => CoordWorkerUpstream::LocalTerminalPeerAnswer(answer),
                    Ok(Err(failure)) => replies::terminal_peer_error(&key, &epoch, failure.as_str()),
                    // v2: an error that is not a TerminalPeerOfferError is `ice_failed`.
                    Err(message) => {
                        tracing::warn!(request_id = %key.request_id, error = %message, "the terminal peer owner failed");
                        replies::terminal_peer_error(&key, &epoch, TerminalPeerOfferFailure::IceFailed.as_str())
                    }
                };
                uplink.send_fenced(&reply_fence, frame);
            }),
        );
    }

    pub(super) fn terminal_peer_cancel(&self, request: &DLocalTerminalPeerCancel) {
        match self.direct() {
            Some(direct) => direct.peer_cancel(request),
            None => tracing::debug!(request_id = %request.request_id, "a peer cancel arrived with no direct terminal owner"),
        }
    }

    /// v2 answers a probe only when its owner returns a result.
    pub(super) fn terminal_transport_probe(&self, request: &DTerminalTransportProbe, link: &mut dyn DownstreamLink) {
        let result = self.direct().and_then(|direct| direct.transport_probe(request));
        match result {
            Some(result) => link.reply(CoordWorkerUpstream::TerminalTransportProbeResult(result)),
            None => tracing::debug!(request_id = %request.request_id, "a transport probe was not for this worker's direct path"),
        }
    }

    pub(super) fn terminal_direct_retire(&self, request: &DTerminalDirectRetire) {
        match self.direct() {
            Some(direct) => direct.direct_retire(request),
            None => tracing::debug!(reason = %request.reason, "a direct retire arrived with no direct terminal owner"),
        }
    }
}
