//! The attachment peer arms: an offer's admission runs now, in receive order,
//! and its answer or refusal goes back fenced to the connection it arrived on;
//! a cancel reaches the owner only for the generation this link accepted.
//! Called by [`super::Dispatcher::dispatch`]. Ports the
//! `localAttachmentPeerOffer`/`localAttachmentPeerCancel` cases of v2
//! `apps/worker/src/transport/coord-link-direct-terminal.ts`.

use std::sync::Arc;
use std::time::Instant;

use roost_proto::{DLocalAttachmentPeerCancel, DLocalAttachmentPeerOffer};
use roost_protocol::attachment_transfer::PeerErrorReason;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use super::owner_task::run_owner;
use super::{Dispatcher, DownstreamLink, replies};
use crate::link_ports::AttachmentPeerPort;
use crate::uplink::RequestBudget;

impl Dispatcher {
    fn attachment_peers(&self) -> Option<Arc<dyn AttachmentPeerPort>> {
        self.owners
            .as_ref()
            .and_then(|owners| owners.attachment_peers.clone())
    }

    /// No owner is v2's absent `onLocalAttachmentPeerOffer` ("disabled"); an
    /// owner refusal carries its own reason; anything else is `ice_failed`.
    pub(super) fn attachment_peer_offer(
        &self,
        request: DLocalAttachmentPeerOffer,
        received: Instant,
        link: &mut dyn DownstreamLink,
    ) {
        let Some(peers) = self.attachment_peers() else {
            link.reply(replies::attachment_peer_error(
                &request,
                &self.process_epoch,
                PeerErrorReason::Disabled,
            ));
            return;
        };
        let refused = request.clone();
        let budget = RequestBudget::from_budget_ms(request.budget_ms, received);
        let fence = self.uplink.fence();
        let reply_fence = fence.clone();
        let uplink = self.uplink.clone();
        let epoch = self.process_epoch.clone();
        run_owner(
            move || peers.offer(request, budget, fence),
            Box::new(move |outcome| {
                let frame = match outcome {
                    Ok(Ok(answer)) => CoordWorkerUpstream::LocalAttachmentPeerAnswer(answer),
                    Ok(Err(reason)) => replies::attachment_peer_error(&refused, &epoch, reason),
                    Err(message) => {
                        tracing::warn!(error = %message, "the attachment peer owner failed");
                        replies::attachment_peer_error(&refused, &epoch, PeerErrorReason::IceFailed)
                    }
                };
                uplink.send_fenced(&reply_fence, frame);
            }),
        );
    }

    /// v2's optional `onLocalAttachmentPeerCancel`: no answer either way.
    pub(super) fn attachment_peer_cancel(&self, request: &DLocalAttachmentPeerCancel) {
        match self.attachment_peers() {
            Some(peers) => peers.cancel(request),
            None => tracing::debug!(
                request_id = %request.request_id,
                "an attachment peer cancel arrived with no attachment peer owner"
            ),
        }
    }
}
