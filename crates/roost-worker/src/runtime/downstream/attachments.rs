//! The downstream attachment arms: a relayed upload chunk, a direct grant and
//! its revocation, and a direct status request — each routed to the attachment
//! owner, or answered as a v2 worker without that owner answers. Called by
//! [`super::Dispatcher::dispatch`]. Ports those cases of v2
//! `apps/worker/src/transport/coord-link-downstream.ts`,
//! `coord-link-direct-terminal.ts` and the reply shapes of `coord-link-deps.ts`.

use std::sync::Arc;

use roost_proto::buffa::MessageField;
use roost_proto::{
    DAttachmentChunk, DAttachmentDirectStatusRequest, DLocalAttachmentGrant,
    DLocalAttachmentGrantRevoke, WAttachmentDirectStatus,
};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use super::owner_task::run_owner;
use super::replies;
use super::{Dispatcher, DownstreamLink};
use crate::attachments::upload::RelayChunkOutcome;

impl Dispatcher {
    /// v2 `onAttachmentChunk`: the bytes are written before the next frame is
    /// read. Only a final chunk or a refusal is answered, on whichever
    /// connection is live when it settles — v2's unfenced `link().send`.
    pub(super) fn attachment_chunk(&self, chunk: DAttachmentChunk) {
        let Some(owners) = &self.owners else {
            tracing::warn!(
                request_id = %chunk.request_id,
                "an attachment chunk arrived with no attachment owner; v2 sends no answer"
            );
            return;
        };
        let request_id = chunk.request_id.clone();
        let uplink = self.uplink.clone();
        let attachments = Arc::clone(&owners.attachments);
        run_owner(
            move || attachments.accept_relay_chunk(chunk),
            Box::new(move |outcome| {
                let frame = match outcome {
                    Ok(RelayChunkOutcome::Progress) => CoordWorkerUpstream::RpcOk { request_id, data: serde_json::json!({}), trace_id: None },
                    Ok(RelayChunkOutcome::Saved { abs_path }) => CoordWorkerUpstream::RpcOk {
                        request_id,
                        data: serde_json::json!({ "abs_path": abs_path }),
                        trace_id: None,
                    },
                    Ok(RelayChunkOutcome::Failed(error)) => {
                        replies::rpc_error(request_id, error.message())
                    }
                    // v2 `handleAttachmentChunk`'s `.catch`: logged, never answered.
                    Err(message) => {
                        tracing::warn!(%request_id, error = %message, "attachment_reply_failed");
                        return;
                    }
                };
                uplink.send(frame);
            }),
        );
    }

    /// v2 `localAttachmentGrant`: installed → `rpc-ok { grant_id }`, refused →
    /// the store's own message.
    pub(super) fn attachment_grant(
        &self,
        request: DLocalAttachmentGrant,
        link: &mut dyn DownstreamLink,
    ) {
        let Some(owners) = &self.owners else {
            link.reply(replies::rpc_error(
                request.request_id,
                replies::LOCAL_ATTACHMENT_GRANTS_UNSUPPORTED,
            ));
            return;
        };
        let reply = match owners.attachments.install_grant(&request) {
            Ok(()) => CoordWorkerUpstream::RpcOk {
                request_id: request.request_id,
                data: serde_json::json!({ "grant_id": request.grant_id }),
                trace_id: None,
            },
            Err(message) => {
                tracing::warn!(request_id = %request.request_id, %message, "an attachment grant was refused");
                replies::rpc_error(request.request_id, message)
            }
        };
        link.reply(reply);
    }

    /// v2 `localAttachmentGrantRevoke`: nothing is answered.
    pub(super) fn attachment_grant_revoke(&self, request: &DLocalAttachmentGrantRevoke) {
        match &self.owners {
            Some(owners) => owners
                .attachments
                .revoke_device(&request.device_fingerprint),
            None => tracing::warn!(
                "an attachment grant revocation arrived with no attachment owner; v2 without that owner sends no answer"
            ),
        }
    }

    /// v2 `attachmentDirectStatusRequest`: the operation's durable status, or
    /// `upload_not_found` from a worker without the owner.
    pub(super) fn attachment_status(
        &self,
        request: DAttachmentDirectStatusRequest,
        link: &mut dyn DownstreamLink,
    ) {
        let Some(owners) = &self.owners else {
            link.reply(replies::attachment_status_unavailable(&request));
            return;
        };
        let status = owners.attachments.direct_status(&request);
        link.reply(CoordWorkerUpstream::AttachmentDirectStatus(
            WAttachmentDirectStatus {
                request_id: request.request_id,
                status: MessageField::some(status),
                ..Default::default()
            },
        ));
    }
}
