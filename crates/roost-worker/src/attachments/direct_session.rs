//! Per-port state for direct attachment uploads and the terminal transitions
//! every refusal ends in: fail with an ack and a closed frame, retire an
//! unacknowledgeable route, close after the terminal frame. `direct_sockets`
//! and `direct_chunks` hold the lock these run under. Ports
//! `apps/worker/src/attachments/attachment-direct-session.ts` and the terminal
//! half of `attachment-direct-socket.ts`.

use std::collections::HashMap;
use std::sync::Arc;

use roost_protocol::attachment_transfer::TransferErrorReason;
use tokio::task::AbortHandle;

use super::direct_frames::{AttachmentAckFields, send_attachment_ack, send_attachment_closed};
use super::transfer_admission::{AttachmentPeerExpectedTuple, AttachmentUploadMetadata};
use super::transfer_lease::AttachmentTransferLease;
use super::transfer_port::AttachmentTransferPort;
use super::upload::AttachmentOperations;

/// One carrier's upload: pre-hello, admitted, or terminal.
#[derive(Debug)]
pub(super) struct AttachmentPortSession {
    pub port: Arc<dyn AttachmentTransferPort>,
    /// Identity within this process: a timer or a finished write that names a
    /// replaced session for the same socket id must not touch the new one.
    pub serial: u64,
    pub expected_peer: Option<AttachmentPeerExpectedTuple>,
    pub metadata: Option<AttachmentUploadMetadata>,
    pub terminal: bool,
    pub lease: Option<AttachmentTransferLease>,
    pub setup_timer: Option<AbortHandle>,
    pub lease_timer: Option<AbortHandle>,
    pub write_pending: bool,
}

impl AttachmentPortSession {
    pub fn new(
        port: Arc<dyn AttachmentTransferPort>,
        serial: u64,
        expected_peer: Option<AttachmentPeerExpectedTuple>,
    ) -> Self {
        Self {
            port,
            serial,
            expected_peer,
            metadata: None,
            terminal: false,
            lease: None,
            setup_timer: None,
            lease_timer: None,
            write_pending: false,
        }
    }

    /// Coordinator-negotiated peers and hello-admitted sockets hold admitted
    /// upload slots; an unauthenticated loopback socket fills only its own.
    pub fn holds_admitted_slot(&self) -> bool {
        self.metadata.is_some() || self.expected_peer.is_some()
    }

    fn stop_timers(&mut self) {
        for timer in [self.setup_timer.take(), self.lease_timer.take()]
            .into_iter()
            .flatten()
        {
            timer.abort();
        }
        self.lease = None;
    }
}

/// Every live session, keyed by the carrier's socket id.
#[derive(Debug, Default)]
pub(super) struct DirectState {
    pub sessions: HashMap<String, AttachmentPortSession>,
    pub disposed: bool,
    pub next_serial: u64,
}

impl DirectState {
    pub fn session(&mut self, socket_id: &str, serial: u64) -> Option<&mut AttachmentPortSession> {
        self.sessions
            .get_mut(socket_id)
            .filter(|session| session.serial == serial)
    }

    pub fn count_sessions(&self, predicate: impl Fn(&AttachmentPortSession) -> bool) -> usize {
        self.sessions
            .values()
            .filter(|session| predicate(session))
            .count()
    }

    /// Takes a session out of the table and stops its timers; the caller
    /// decides what the port hears.
    pub fn remove_session(
        &mut self,
        socket_id: &str,
        serial: u64,
    ) -> Option<AttachmentPortSession> {
        self.session(socket_id, serial)?;
        let mut session = self.sessions.remove(socket_id)?;
        session.stop_timers();
        Some(session)
    }

    /// A refusal the browser hears: an ack naming the reason once an upload
    /// was admitted, then a closed frame, then the carrier drains and closes.
    pub fn fail(
        &mut self,
        operations: &AttachmentOperations,
        socket_id: &str,
        serial: u64,
        refusal: Refusal<'_>,
    ) {
        let Some(session) = self.session(socket_id, serial) else {
            return;
        };
        if session.terminal {
            return;
        }
        session.terminal = true;
        let port = Arc::clone(&session.port);
        let reason = refusal.reason.as_str();
        if let Some(metadata) = &session.metadata {
            let bytes_received = operations
                .status(&metadata.session_id, &metadata.upload_id)
                .bytes_received;
            let ack = AttachmentAckFields {
                seq: refusal.seq,
                bytes_received,
                chunk_sha256: refusal.chunk_sha256,
            };
            send_attachment_ack(port.as_ref(), &metadata.upload_id, ack, "", reason);
            operations.detach_direct_carrier(socket_id);
        }
        send_attachment_closed(port.as_ref(), reason);
        self.close_after_terminal_frame(socket_id, serial, reason);
        tracing::info!(
            carrier = port.kind().as_str(),
            reason,
            "attachment upload failed"
        );
    }

    /// A route whose frames could not be written at all: no closed frame can
    /// reach the browser, so only the carrier's own close tells it.
    pub fn retire_unacknowledged_route(
        &mut self,
        socket_id: &str,
        serial: u64,
        operations: &AttachmentOperations,
        reason: &str,
    ) {
        let Some(session) = self.session(socket_id, serial) else {
            return;
        };
        session.terminal = true;
        if session.metadata.is_some() {
            operations.detach_direct_carrier(socket_id);
        }
        self.close_after_terminal_frame(socket_id, serial, reason);
        tracing::info!(reason, "attachment route retired unacknowledged");
    }

    pub fn close_after_terminal_frame(&mut self, socket_id: &str, serial: u64, reason: &str) {
        if let Some(session) = self.remove_session(socket_id, serial) {
            session.port.close_after_drain(reason);
        }
    }
}

/// What a failure ack names: the reason, and the chunk it answers when a
/// chunk caused it (sequence zero and an empty digest otherwise).
#[derive(Debug, Clone, Copy)]
pub(super) struct Refusal<'refusal> {
    pub reason: TransferErrorReason,
    pub seq: u32,
    pub chunk_sha256: &'refusal str,
}

impl Refusal<'static> {
    pub fn bare(reason: TransferErrorReason) -> Self {
        Self {
            reason,
            seq: 0,
            chunk_sha256: "",
        }
    }
}
