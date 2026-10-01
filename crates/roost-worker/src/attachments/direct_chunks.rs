//! The admitted half of a direct upload: a chunk is checked under the lock,
//! written by the operation owner off it, and acknowledged — or refused — once
//! the write settles; a status request answers lost-ACK recovery. Called by
//! `direct_sockets`. Ports `acceptChunk`, `acceptStatusRequest` and the
//! acknowledgement half of `apps/worker/src/attachments/attachment-direct-socket.ts`.

use std::time::Instant;

use roost_proto::{AttachmentTransferChunk, AttachmentTransferStatusRequest};
use roost_protocol::attachment_transfer::{
    COMPLETE_REASON, DIRECT_CHUNK_BYTES, TransferErrorReason,
};

use super::direct_frames::{
    AttachmentAckFields, send_attachment_ack, send_attachment_closed, send_attachment_status,
};
use super::direct_session::{DirectState, Refusal};
use super::receipts::AttachmentOperationReceipt;
use super::upload::{AttachmentOperations, DirectChunk, DirectChunkOutcome};

/// The largest offset v2 could represent exactly; beyond it the chunk's
/// position is unknowable and is refused as a mismatch.
const MAX_SAFE_OFFSET: u64 = (1 << 53) - 1;

/// Which session a settled write answers, and the chunk it wrote.
#[derive(Debug, Clone)]
pub(super) struct WriteTicket {
    pub socket_id: String,
    pub serial: u64,
    pub seq: u32,
    pub chunk_sha256: String,
}

impl DirectState {
    /// The synchronous half of a chunk: every refusal that needs no write, then
    /// the write marked pending so a second frame cannot overtake it.
    pub fn begin_chunk(
        &mut self,
        operations: &AttachmentOperations,
        socket_id: &str,
        serial: u64,
        chunk: AttachmentTransferChunk,
    ) -> Option<(WriteTicket, DirectChunk)> {
        let session = self.session(socket_id, serial)?;
        let metadata = session.metadata.clone()?;
        let active = session
            .lease
            .as_mut()
            .is_some_and(|lease| lease.allows_activity(Instant::now()));
        if !active {
            self.fail(
                operations,
                socket_id,
                serial,
                Refusal::bare(TransferErrorReason::GrantUnavailable),
            );
            return None;
        }
        let refusal = if chunk.upload_id != metadata.upload_id {
            Some(TransferErrorReason::UploadMismatch)
        } else if chunk.offset > MAX_SAFE_OFFSET {
            Some(TransferErrorReason::ChunkOffsetMismatch)
        } else if chunk.data.len() > DIRECT_CHUNK_BYTES {
            Some(TransferErrorReason::ChunkTooLarge)
        } else {
            None
        };
        if let Some(reason) = refusal {
            let refusal = Refusal {
                reason,
                seq: chunk.seq,
                chunk_sha256: &chunk.chunk_sha256,
            };
            self.fail(operations, socket_id, serial, refusal);
            return None;
        }
        session.write_pending = true;
        let ticket = WriteTicket {
            socket_id: socket_id.to_owned(),
            serial,
            seq: chunk.seq,
            chunk_sha256: chunk.chunk_sha256.clone(),
        };
        let write = DirectChunk {
            upload_id: metadata.upload_id,
            session_id: metadata.session_id,
            filename: metadata.filename,
            short_path: metadata.short_path,
            total_bytes: metadata.total_bytes,
            data: chunk.data,
            last: chunk.last,
            seq: chunk.seq,
            offset: chunk.offset,
            chunk_sha256: chunk.chunk_sha256,
            carrier_id: socket_id.to_owned(),
        };
        Some((ticket, write))
    }

    /// The settled half: progress and completion are acknowledged only while
    /// the lease still stands; an operation error is the browser's refusal.
    pub fn settle_chunk(
        &mut self,
        operations: &AttachmentOperations,
        ticket: &WriteTicket,
        outcome: DirectChunkOutcome,
    ) {
        let Some(session) = self.session(&ticket.socket_id, ticket.serial) else {
            return;
        };
        session.write_pending = false;
        match outcome {
            DirectChunkOutcome::Progress(receipt) => {
                if self.note_activity(operations, ticket) {
                    self.acknowledge(operations, ticket, &receipt, "");
                }
            }
            DirectChunkOutcome::Committed { abs_path, receipt } => {
                if self.note_activity(operations, ticket) {
                    self.acknowledge_completion(ticket, &receipt, &abs_path);
                }
            }
            DirectChunkOutcome::Failed(error) => {
                let refusal = Refusal {
                    reason: error.reason(),
                    seq: ticket.seq,
                    chunk_sha256: &ticket.chunk_sha256,
                };
                self.fail(operations, &ticket.socket_id, ticket.serial, refusal);
            }
        }
    }

    /// Status is lost-ACK recovery, not progress: it never keeps an idle port
    /// alive.
    pub fn accept_status_request(
        &mut self,
        operations: &AttachmentOperations,
        socket_id: &str,
        serial: u64,
        request: &AttachmentTransferStatusRequest,
    ) {
        let Some(session) = self.session(socket_id, serial) else {
            return;
        };
        let Some(metadata) = session
            .metadata
            .clone()
            .filter(|metadata| metadata.upload_id == request.upload_id)
        else {
            self.fail(
                operations,
                socket_id,
                serial,
                Refusal::bare(TransferErrorReason::UploadMismatch),
            );
            return;
        };
        let active = session
            .lease
            .as_mut()
            .is_some_and(|lease| lease.allows_activity(Instant::now()));
        if !active {
            self.fail(
                operations,
                socket_id,
                serial,
                Refusal::bare(TransferErrorReason::GrantUnavailable),
            );
            return;
        }
        let status = operations.status(&metadata.session_id, &metadata.upload_id);
        if !send_attachment_status(session.port.as_ref(), &status) {
            self.retire_unacknowledged_route(socket_id, serial, operations, "write_failed");
        }
    }

    /// A lease that has lapsed fails the upload as v2's expiry callback does;
    /// a session whose lease is already gone was retired and hears nothing.
    fn note_activity(&mut self, operations: &AttachmentOperations, ticket: &WriteTicket) -> bool {
        let Some(session) = self.session(&ticket.socket_id, ticket.serial) else {
            return false;
        };
        let Some(lease) = session.lease.as_mut() else {
            return false;
        };
        if lease.note_valid_activity(Instant::now()) {
            return true;
        }
        let refusal = Refusal::bare(TransferErrorReason::GrantUnavailable);
        self.fail(operations, &ticket.socket_id, ticket.serial, refusal);
        false
    }

    fn acknowledge(
        &mut self,
        operations: &AttachmentOperations,
        ticket: &WriteTicket,
        receipt: &AttachmentOperationReceipt,
        abs_path: &str,
    ) {
        if !self.send_ack(ticket, receipt, abs_path) {
            self.retire_unacknowledged_route(
                &ticket.socket_id,
                ticket.serial,
                operations,
                "write_failed",
            );
        }
    }

    fn acknowledge_completion(
        &mut self,
        ticket: &WriteTicket,
        receipt: &AttachmentOperationReceipt,
        abs_path: &str,
    ) {
        if !self.send_ack(ticket, receipt, abs_path) {
            self.close_after_terminal_frame(&ticket.socket_id, ticket.serial, "write_failed");
            return;
        }
        let Some(session) = self.session(&ticket.socket_id, ticket.serial) else {
            return;
        };
        session.terminal = true;
        let port = std::sync::Arc::clone(&session.port);
        send_attachment_closed(port.as_ref(), COMPLETE_REASON);
        // v2's own line is `log.info("attachment-transfer", "upload_completed",
        // { carrier })` — the event NAME is the message, and the facade renders
        // `msg` before the caller's fields. That adjacency is what the oracle
        // greps for, and it is v2's real output rather than a quirk of it, so
        // the name belongs in the message where it was. The line renders
        // `"msg":"upload_completed","carrier":"loopback"`.
        tracing::info!(carrier = port.kind().as_str(), "upload_completed");
    }

    fn send_ack(
        &mut self,
        ticket: &WriteTicket,
        receipt: &AttachmentOperationReceipt,
        abs_path: &str,
    ) -> bool {
        let Some(session) = self.session(&ticket.socket_id, ticket.serial) else {
            return false;
        };
        let Some(metadata) = &session.metadata else {
            return false;
        };
        let ack = AttachmentAckFields {
            seq: receipt.seq,
            bytes_received: receipt.bytes_received,
            chunk_sha256: &receipt.chunk_sha256,
        };
        send_attachment_ack(
            session.port.as_ref(),
            &metadata.upload_id,
            ack,
            abs_path,
            "",
        )
    }
}
