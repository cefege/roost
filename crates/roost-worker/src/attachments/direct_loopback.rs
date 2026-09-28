//! The loopback attachment carrier: the port over one door WebSocket on the
//! attachment subprotocol, and the handlers the door's shared loopback pump
//! drives for that route. Protobuf admission, acknowledgements and close
//! semantics stay in `direct_sockets`. Ports
//! `apps/worker/src/attachments/local-ui-attachment-socket.ts`; mounted by
//! `runtime::owners` as the door's attachment `LoopbackOwner`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use roost_protocol::attachment_transfer::{LOOPBACK_MAX_PAYLOAD_BYTES, PeerChannelLane};

use super::direct_sockets::{AttachmentDirectSockets, DirectLane};
use super::transfer_port::{AttachmentTransferPort, PortKind, SendResult};
use crate::door::loopback::LoopbackHandlers;
use crate::door::{LoopbackSend, LoopbackSocket};

/// How much may sit unsent before a send is refused rather than backpressured.
const MAX_BACKPRESSURE_BYTES: usize = LOOPBACK_MAX_PAYLOAD_BYTES * 4;

/// The code a close without one carries; loopback callers always name theirs.
const NORMAL_CLOSURE: u16 = 1000;

#[derive(Debug)]
pub struct LoopbackAttachmentTransferPort {
    socket_id: String,
    socket: LoopbackSocket,
    backpressured_bytes: AtomicUsize,
}

impl LoopbackAttachmentTransferPort {
    pub fn new(socket_id: String, socket: LoopbackSocket) -> Self {
        Self {
            socket_id,
            socket,
            backpressured_bytes: AtomicUsize::new(0),
        }
    }
}

impl AttachmentTransferPort for LoopbackAttachmentTransferPort {
    fn socket_id(&self) -> &str {
        &self.socket_id
    }

    fn kind(&self) -> PortKind {
        PortKind::Loopback
    }

    fn is_open(&self) -> bool {
        self.socket.is_open()
    }

    /// One socket carries both lanes, so the lane is not consulted.
    fn send(&self, bytes: Vec<u8>, _lane: PeerChannelLane) -> SendResult {
        if !self.socket.is_open() {
            return SendResult::Refused;
        }
        let length = bytes.len();
        match self.socket.send(bytes) {
            LoopbackSend::Written => {
                self.backpressured_bytes.store(0, Ordering::Release);
                SendResult::Accepted
            }
            LoopbackSend::Queued => {
                let queued = self.backpressured_bytes.fetch_add(length, Ordering::AcqRel) + length;
                if queued <= MAX_BACKPRESSURE_BYTES {
                    SendResult::Backpressured
                } else {
                    SendResult::Refused
                }
            }
            LoopbackSend::Dropped => SendResult::Refused,
        }
    }

    fn close(&self, code: Option<u16>, reason: &str) {
        self.socket.close(code.unwrap_or(NORMAL_CLOSURE), reason);
    }

    fn close_after_drain(&self, reason: &str) {
        self.close(Some(NORMAL_CLOSURE), reason);
    }

    /// A loopback socket has one lane: there is no data channel for an
    /// unauthenticated peer to write before its hello is admitted.
    fn mark_authenticated(&self) {}
}

impl LoopbackHandlers for AttachmentDirectSockets {
    type Port = LoopbackAttachmentTransferPort;

    fn port(&self, socket_id: String, socket: LoopbackSocket) -> Arc<Self::Port> {
        Arc::new(LoopbackAttachmentTransferPort::new(socket_id, socket))
    }

    fn on_open(&self, port: &Arc<Self::Port>) -> anyhow::Result<()> {
        let port: Arc<dyn AttachmentTransferPort> =
            Arc::clone(port) as Arc<dyn AttachmentTransferPort>;
        self.open_loopback_port(port);
        Ok(())
    }

    /// The write is spawned, not awaited: the pump reads the next frame while
    /// it runs, and a frame that arrives before its ack is refused as out of
    /// order by the synchronous half that already ran.
    fn on_message(&self, port: &Arc<Self::Port>, bytes: Vec<u8>) -> anyhow::Result<()> {
        if let Some(write) = self.receive_frame(port.socket_id(), DirectLane::Loopback, &bytes) {
            tokio::spawn(write);
        }
        Ok(())
    }

    fn on_close(&self, port: &Arc<Self::Port>) -> anyhow::Result<()> {
        self.close_port(port.socket_id());
        Ok(())
    }
}
