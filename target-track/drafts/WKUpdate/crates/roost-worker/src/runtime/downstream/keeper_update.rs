//! The `keeperUpdatePrepare` arm: the preparation owner answers later, with an
//! `rpc-ok` carrying its result or an `rpc-error` carrying its failure, on the
//! connection the request arrived on. Called by
//! [`super::Dispatcher::dispatch`]. Ports the `keeperUpdatePrepare` case of v2
//! `apps/worker/src/transport/coord-link-downstream.ts`.

use std::sync::Arc;

use roost_proto::DKeeperUpdatePrepare;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use super::owner_task::run_owner;
use super::{Dispatcher, DownstreamLink, replies};

impl Dispatcher {
    /// With no owner, v2's own absent-callback answer; otherwise the owner's
    /// synchronous half runs now (it closes channel admission in receive
    /// order) and its outcome is sent when it settles.
    pub(super) fn keeper_update_prepare(
        &self,
        request: DKeeperUpdatePrepare,
        link: &mut dyn DownstreamLink,
    ) {
        let Some(owners) = &self.owners else {
            link.reply(replies::rpc_error(
                request.request_id,
                replies::KEEPER_UPDATE_PREPARE_UNSUPPORTED,
            ));
            return;
        };
        let preparer = Arc::clone(&owners.keeper_update);
        let request_id = request.request_id.clone();
        let fence = self.uplink.fence();
        let uplink = self.uplink.clone();
        tracing::info!(%request_id, "a keeper update preparation was routed to its owner");
        run_owner(
            move || preparer.prepare(request),
            Box::new(move |outcome| {
                let frame = match outcome {
                    Ok(data) => CoordWorkerUpstream::RpcOk {
                        request_id,
                        data,
                        trace_id: None,
                    },
                    Err(message) => replies::rpc_error(request_id, message),
                };
                uplink.send_fenced(&fence, frame);
            }),
        );
    }
}
