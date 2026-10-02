//! The attachment loopback carrier: one `WebSocket` to the matching worker
//! door, authenticated by the upload's grant, carrying whole frames.
//!
//! Opened by `upload_host` when the discovered door names the session's worker.
//! The frame rules — hello, Ready admission, ack and status settlement — are
//! `client::attachments::direct::loopback::LoopbackTransfer`'s; this owns the
//! socket, its callbacks and the three deadlines. Ports
//! `apps/web/src/client/carriers/attachment-loopback.ts`.

use js_sys::Uint8Array;
use roost_client_core::client::attachments::conversation::ConversationOutcome;
use roost_client_core::client::attachments::direct::loopback::{
    ACK_DEADLINE_MS, LOOPBACK_SUBPROTOCOL, LoopbackDeadline, LoopbackTransfer, SETUP_DEADLINE_MS,
    STATUS_DEADLINE_MS, attachment_loopback_url,
};
use roost_client_core::client::attachments::grant::AttachmentDirectGrant;
use roost_client_core::client::attachments::transfer::receipt::AttachmentTransferStatus;
use roost_client_core::client::attachments::transfer::{
    AttachmentTransferAck, AttachmentTransferCarrierError, InFlightChunk,
};
use roost_client_core::client::local::discovery::LocalWorkerDoor;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use web_sys::{BinaryType, MessageEvent, WebSocket};

use super::AttachmentCarrier;
use super::inbox::{Deadline, EventInbox};

/// One thing the socket's callbacks observed.
#[derive(Debug)]
enum SocketEvent {
    Open,
    Frame(Vec<u8>),
    /// A text frame, which no attachment door sends.
    Invalid,
    Failed,
    Closed,
}

/// The browser callbacks, kept alive for as long as the socket can fire them.
#[derive(Debug)]
struct SocketListeners {
    _on_open: Closure<dyn FnMut(JsValue)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut(JsValue)>,
    _on_close: Closure<dyn FnMut(JsValue)>,
}

/// An authenticated loopback socket for one upload.
#[derive(Debug)]
pub struct AttachmentLoopbackCarrier {
    socket: WebSocket,
    inbox: EventInbox<SocketEvent>,
    transfer: LoopbackTransfer,
    released: bool,
    _listeners: SocketListeners,
}

impl AttachmentLoopbackCarrier {
    /// Open the door's attachment socket and wait for the worker's `Ready`.
    ///
    /// Every failure here is a refusal with `sent_chunk == false`: no chunk
    /// can leave before `Ready`, so the route chooser may still try the peer.
    pub async fn open(
        door: &LocalWorkerDoor,
        grant: &AttachmentDirectGrant,
    ) -> Result<Self, AttachmentTransferCarrierError> {
        let mut transfer = LoopbackTransfer::new(&door.worker_fingerprint, grant.clone());
        let url = attachment_loopback_url(&door.origin);
        let Ok(socket) = WebSocket::new_with_str(&url, LOOPBACK_SUBPROTOCOL) else {
            return Err(transfer.close("attachment loopback could not open"));
        };
        socket.set_binary_type(BinaryType::Arraybuffer);
        let inbox = EventInbox::new();
        let listeners = install_listeners(&socket, &inbox);
        let mut carrier = Self {
            socket,
            inbox,
            transfer,
            released: false,
            _listeners: listeners,
        };
        let deadline = Deadline::after(SETUP_DEADLINE_MS);
        loop {
            let failure = match carrier.inbox.next(&deadline).await {
                None => Some(carrier.elapsed(LoopbackDeadline::Setup)),
                Some(SocketEvent::Open) => match carrier.transfer.socket_opened() {
                    Ok(hello) if carrier.write(&hello) => None,
                    Ok(_) => Some(
                        carrier
                            .transfer
                            .close("attachment loopback could not authenticate"),
                    ),
                    Err(error) => Some(error),
                },
                Some(SocketEvent::Frame(bytes)) => match carrier.transfer.frame_received(&bytes) {
                    Ok(ConversationOutcome::Ready) => {
                        tracing::info!(
                            target: "attachments",
                            upload = %grant.request.upload_id,
                            "attachment loopback carrier ready"
                        );
                        return Ok(carrier);
                    }
                    Ok(_) => None,
                    Err(error) => Some(error),
                },
                Some(other) => Some(carrier.end_on(other)),
            };
            if let Some(error) = failure {
                carrier.release();
                return Err(error);
            }
        }
    }

    /// Write one encoded client frame. `false` means it did not go out.
    fn write(&self, frame: &[u8]) -> bool {
        !self.released
            && self.socket.ready_state() == WebSocket::OPEN
            && self.socket.send_with_u8_array(frame).is_ok()
    }

    /// The carrier error a lifecycle event ends this socket with.
    fn end_on(&mut self, event: SocketEvent) -> AttachmentTransferCarrierError {
        let reason = match event {
            SocketEvent::Invalid => "attachment loopback received an invalid frame",
            SocketEvent::Failed => "attachment loopback failed",
            SocketEvent::Closed | SocketEvent::Open | SocketEvent::Frame(_) => {
                "attachment loopback closed"
            }
        };
        self.transfer.close(reason)
    }

    /// The carrier error an elapsed deadline settles its waiter with.
    fn elapsed(&mut self, deadline: LoopbackDeadline) -> AttachmentTransferCarrierError {
        match self.transfer.deadline_elapsed(deadline) {
            Err(error) => error,
            Ok(()) => self.transfer.close("attachment loopback deadline elapsed"),
        }
    }

    /// Unhook the callbacks and close the socket.
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onerror(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close();
    }
}

impl AttachmentCarrier for AttachmentLoopbackCarrier {
    async fn send_chunk(
        &mut self,
        chunk: &InFlightChunk,
        data: Vec<u8>,
    ) -> Result<AttachmentTransferAck, AttachmentTransferCarrierError> {
        let frame = self.transfer.send_chunk(chunk, data)?;
        if !self.write(&frame) {
            return Err(self
                .transfer
                .send_failed("attachment loopback could not send a chunk"));
        }
        let deadline = Deadline::after(ACK_DEADLINE_MS);
        loop {
            let failure = match self.inbox.next(&deadline).await {
                // Ambiguous and NOT closed: the socket stays up, because the
                // receipt that settles this chunk is asked for on it next.
                None => return Err(self.elapsed(LoopbackDeadline::Ack)),
                Some(SocketEvent::Frame(bytes)) => match self.transfer.frame_received(&bytes) {
                    Ok(ConversationOutcome::Ack(ack)) => return Ok(ack),
                    Ok(_) => continue,
                    Err(error) => error,
                },
                Some(other) => self.end_on(other),
            };
            if !failure.ambiguous || self.transfer.is_closed() {
                self.release();
            }
            return Err(failure);
        }
    }

    async fn request_status(
        &mut self,
        upload_id: &str,
    ) -> Result<AttachmentTransferStatus, AttachmentTransferCarrierError> {
        let frame = self.transfer.request_status(upload_id)?;
        if !self.write(&frame) {
            return Err(self
                .transfer
                .close("attachment loopback could not request status"));
        }
        let deadline = Deadline::after(STATUS_DEADLINE_MS);
        loop {
            let failure = match self.inbox.next(&deadline).await {
                None => return Err(self.elapsed(LoopbackDeadline::Status)),
                Some(SocketEvent::Frame(bytes)) => match self.transfer.frame_received(&bytes) {
                    Ok(ConversationOutcome::Status(status)) => return Ok(*status),
                    Ok(_) => continue,
                    Err(error) => error,
                },
                Some(other) => self.end_on(other),
            };
            if self.transfer.is_closed() {
                self.release();
            }
            return Err(failure);
        }
    }

    fn sent_chunk(&self) -> bool {
        self.transfer.sent_chunk()
    }

    fn close(&mut self, reason: &str) {
        let _ = self.transfer.close(reason);
        self.release();
    }
}

impl Drop for AttachmentLoopbackCarrier {
    fn drop(&mut self) {
        self.release();
    }
}

/// Route the socket's four callbacks into `inbox`.
fn install_listeners(socket: &WebSocket, inbox: &EventInbox<SocketEvent>) -> SocketListeners {
    let on_open = {
        let inbox = inbox.clone();
        Closure::wrap(
            Box::new(move |_event: JsValue| inbox.push(SocketEvent::Open))
                as Box<dyn FnMut(JsValue)>,
        )
    };
    let on_message = {
        let inbox = inbox.clone();
        Closure::wrap(Box::new(move |event: MessageEvent| {
            let observed = match event.data().dyn_into::<js_sys::ArrayBuffer>() {
                Ok(buffer) => SocketEvent::Frame(Uint8Array::new(&buffer).to_vec()),
                Err(_) => SocketEvent::Invalid,
            };
            inbox.push(observed);
        }) as Box<dyn FnMut(MessageEvent)>)
    };
    let on_error = {
        let inbox = inbox.clone();
        Closure::wrap(
            Box::new(move |_event: JsValue| inbox.push(SocketEvent::Failed))
                as Box<dyn FnMut(JsValue)>,
        )
    };
    let on_close = {
        let inbox = inbox.clone();
        Closure::wrap(
            Box::new(move |_event: JsValue| inbox.push(SocketEvent::Closed))
                as Box<dyn FnMut(JsValue)>,
        )
    };
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    SocketListeners {
        _on_open: on_open,
        _on_message: on_message,
        _on_error: on_error,
        _on_close: on_close,
    }
}
