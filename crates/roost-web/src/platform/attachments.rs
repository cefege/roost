//! The browser halves of the two direct attachment carriers: the loopback
//! socket and the attachment WebRTC peer, and the chunk loop both run.
//!
//! Owned by `platform`; driven by `components::terminal_chrome::upload_host`,
//! whose route chooser (`client::attachments::direct`) decides which carrier
//! runs. The frame rules are the client core's (`direct::loopback`, `peer`,
//! `transfer`); this module owns the sockets, the timers and the awaiting.
//! Ported from `apps/web/src/client/carriers/attachment-loopback.ts`,
//! `apps/web/src/client/attachments/attachmentPeer.ts` and `attachmentTransfer.ts`.

pub mod inbox;
pub mod loopback;
pub mod peer;
pub mod peer_objects;
pub mod send_file;

use roost_client_core::client::attachments::transfer::receipt::AttachmentTransferStatus;
use roost_client_core::client::attachments::transfer::{
    AttachmentTransferAck, AttachmentTransferCarrierError, InFlightChunk,
};

/// One opened direct carrier, as the chunk loop drives it: one chunk or one
/// receipt in flight at a time, and a close once the upload settles.
pub(crate) trait AttachmentCarrier {
    /// Send one framed chunk and await its acknowledgement.
    async fn send_chunk(
        &mut self,
        chunk: &InFlightChunk,
        data: Vec<u8>,
    ) -> Result<AttachmentTransferAck, AttachmentTransferCarrierError>;

    /// Ask the worker, over this carrier, what it durably holds.
    async fn request_status(
        &mut self,
        upload_id: &str,
    ) -> Result<AttachmentTransferStatus, AttachmentTransferCarrierError>;

    /// Whether any upload byte has left on this carrier.
    fn sent_chunk(&self) -> bool;

    /// Release the carrier. Idempotent: the outcome is already settled.
    fn close(&mut self, reason: &str);
}
