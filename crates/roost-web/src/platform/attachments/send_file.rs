//! The direct chunk loop: one opened carrier, the file in 512 KiB slices, a
//! per-chunk digest, and the receipt recovery a lost acknowledgement needs.
//!
//! Called by `upload_host`'s carrier routes once a carrier is open. The slice
//! and settle rules are `client::attachments::transfer::DirectUpload`'s and the
//! receipt rule is `transfer::receipt::settle_from_receipt`; this file performs
//! the awaits between them. Ports `sendAttachmentFile` in `attachmentTransfer.ts`.

use roost_client_core::client::attachments::transfer::receipt::{
    AttachmentTransferStatus, ReceiptOutcome, ReceiptSource, settle_from_receipt,
};
use roost_client_core::client::attachments::transfer::{
    AttachmentTransferAck, AttachmentTransferCarrierError, AttachmentTransferResult, DirectUpload,
    InFlightChunk,
};

use super::AttachmentCarrier;
use crate::components::terminal_chrome::upload_id::content_digest;

/// Send `bytes` as one ordered direct upload on `carrier`, then close it.
///
/// `coordinator_status` is the receipt of last resort: a carrier that lost an
/// acknowledgement may also have lost the socket that could answer for it, and
/// the coordinator relays the worker's durable view.
pub(crate) async fn send_attachment_file<C: AttachmentCarrier>(
    carrier: &mut C,
    upload_id: &str,
    bytes: &[u8],
    on_progress: &dyn Fn(u64),
    coordinator_status: impl AsyncFn(&str) -> Option<AttachmentTransferStatus>,
) -> Result<AttachmentTransferResult, AttachmentTransferCarrierError> {
    let mut upload = DirectUpload::new(upload_id, bytes.len() as u64);
    let settled = send_every_chunk(
        carrier,
        &mut upload,
        bytes,
        on_progress,
        &coordinator_status,
    )
    .await;
    carrier.close(upload.close_reason());
    settled
}

async fn send_every_chunk<C: AttachmentCarrier>(
    carrier: &mut C,
    upload: &mut DirectUpload,
    bytes: &[u8],
    on_progress: &dyn Fn(u64),
    coordinator_status: &impl AsyncFn(&str) -> Option<AttachmentTransferStatus>,
) -> Result<AttachmentTransferResult, AttachmentTransferCarrierError> {
    while let Some(slice) = upload.next_slice() {
        let start = usize::try_from(slice.offset).map_err(|_| unreadable(carrier))?;
        let data = bytes
            .get(start..start + slice.bytes)
            .ok_or_else(|| unreadable(carrier))?
            .to_vec();
        let digest = content_digest(&data).await.map_err(|_| {
            AttachmentTransferCarrierError::refused(
                "attachment transfer could not hash a chunk",
                carrier.sent_chunk(),
            )
        })?;
        upload.begin_chunk(&data, &digest).map_err(|refusal| {
            AttachmentTransferCarrierError::refused(&refusal.to_string(), carrier.sent_chunk())
        })?;
        let chunk = upload
            .in_flight()
            .cloned()
            .ok_or_else(|| unreadable(carrier))?;
        let ack = match carrier.send_chunk(&chunk, data).await {
            Ok(ack) => ack,
            Err(error) if error.ambiguous => {
                recover_acknowledgement(carrier, &chunk, coordinator_status).await?
            }
            Err(error) => return Err(error),
        };
        let settled = upload.settle(&ack)?;
        on_progress(settled.bytes_received);
    }
    upload.outcome().ok_or_else(|| unreadable(carrier))
}

/// Settle a chunk whose acknowledgement was lost from a durable receipt: the
/// carrier's own first, then the coordinator's.
async fn recover_acknowledgement<C: AttachmentCarrier>(
    carrier: &mut C,
    chunk: &InFlightChunk,
    coordinator_status: &impl AsyncFn(&str) -> Option<AttachmentTransferStatus>,
) -> Result<AttachmentTransferAck, AttachmentTransferCarrierError> {
    if let Ok(status) = carrier.request_status(&chunk.upload_id).await
        && let ReceiptOutcome::Settled(ack) =
            settle_from_receipt(chunk, ReceiptSource::Carrier, Some(&status))
    {
        return Ok(ack);
    }
    let status = coordinator_status(&chunk.upload_id).await;
    match settle_from_receipt(chunk, ReceiptSource::Coordinator, status.as_ref()) {
        ReceiptOutcome::Settled(ack) => Ok(ack),
        ReceiptOutcome::TryNextSource | ReceiptOutcome::Unconfirmed => {
            Err(AttachmentTransferCarrierError::unconfirmed())
        }
    }
}

/// The file could not be sliced where the upload said it would be, which is a
/// browser that changed the bytes under the loop.
fn unreadable<C: AttachmentCarrier>(carrier: &C) -> AttachmentTransferCarrierError {
    AttachmentTransferCarrierError::refused(
        "attachment transfer could not read a chunk",
        carrier.sent_chunk(),
    )
}
