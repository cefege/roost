//! The local-terminal grant arms of the downstream dispatch: an acknowledged
//! install answers `rpc-ok` with the grant id or `rpc-error` with the store's
//! refusal (a browser is told the fast path exists only after this ack), and a
//! revocation fences the device. Called by `super::Dispatcher::dispatch`. Ports
//! the `localTerminalGrant`/`localTerminalGrantRevoke` cases of
//! `apps/worker/src/transport/coord-link-downstream.ts` and their callbacks in
//! `transport/coord-link-deps.ts`.

use roost_proto::{DLocalTerminalGrant, DLocalTerminalGrantRevoke};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use super::{DownstreamLink, Dispatcher, replies};

impl Dispatcher {
    pub(super) fn local_terminal_grant(&self, request: DLocalTerminalGrant, link: &mut dyn DownstreamLink) {
        let Some(owners) = &self.owners else {
            link.reply(replies::rpc_error(request.request_id, replies::LOCAL_TERMINAL_GRANTS_UNSUPPORTED));
            return;
        };
        match owners.local_terminal.install_grant(&request) {
            Ok(()) => link.reply(CoordWorkerUpstream::RpcOk {
                request_id: request.request_id,
                data: serde_json::json!({ "grant_id": request.grant_id }),
                trace_id: None,
            }),
            Err(message) => {
                tracing::warn!(grant_id = %request.grant_id, %message, "a local terminal grant install was refused");
                link.reply(replies::rpc_error(request.request_id, message));
            }
        }
    }

    pub(super) fn local_terminal_revoke(&self, request: &DLocalTerminalGrantRevoke) {
        match &self.owners {
            Some(owners) => owners.local_terminal.revoke_device(&request.device_fingerprint),
            None => tracing::debug!("a local terminal grant revocation reached a worker without a local door"),
        }
    }
}
