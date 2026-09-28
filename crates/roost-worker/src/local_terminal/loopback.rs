//! The loopback terminal port over the door's native WebSocket, and the socket
//! owner's side of the door's handler contract. `crate::door::loopback` owns
//! routing and the socket lifecycle; `LocalTerminalSockets` owns frame
//! admission; this adapter only maps native send ownership onto the common
//! port. Ports `apps/worker/src/local-door/local-ui-terminal-socket.ts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use roost_protocol::terminal_peer::peer::TerminalPeerPacketLane;

use super::port::{PacketSendResult, TerminalPacketPort};
use super::sockets::LocalTerminalSockets;
use crate::door::loopback::LoopbackHandlers;
use crate::door::{LOCAL_TERMINAL_MAX_BACKPRESSURE_BYTES, LoopbackSend, LoopbackSocket};

/// v2 `LoopbackTerminalPacketPort`: every lane is the one socket, and bytes
/// queued behind unflushed ones count against the socket's own bound.
#[derive(Debug)]
pub struct LoopbackTerminalPacketPort {
    socket_id: String,
    socket: LoopbackSocket,
    max_backpressure_bytes: usize,
    backpressured_bytes: AtomicUsize,
}

impl LoopbackTerminalPacketPort {
    pub fn new(socket_id: String, socket: LoopbackSocket, max_backpressure_bytes: usize) -> Self {
        Self {
            socket_id,
            socket,
            max_backpressure_bytes,
            backpressured_bytes: AtomicUsize::new(0),
        }
    }
}

impl TerminalPacketPort for LoopbackTerminalPacketPort {
    fn socket_id(&self) -> &str {
        &self.socket_id
    }

    fn is_open(&self) -> bool {
        self.socket.is_open()
    }

    fn send(&self, bytes: Vec<u8>, _lane: TerminalPeerPacketLane) -> PacketSendResult {
        if !self.socket.is_open() {
            return PacketSendResult::Refused;
        }
        let length = bytes.len();
        match self.socket.send(bytes) {
            LoopbackSend::Written => {
                self.backpressured_bytes.store(0, Ordering::Release);
                PacketSendResult::Accepted
            }
            LoopbackSend::Queued => {
                let held = self.backpressured_bytes.fetch_add(length, Ordering::AcqRel) + length;
                if held <= self.max_backpressure_bytes {
                    PacketSendResult::Backpressured
                } else {
                    PacketSendResult::Refused
                }
            }
            LoopbackSend::Dropped => PacketSendResult::Refused,
        }
    }

    fn close(&self, code: u16, reason: &str) {
        self.socket.close(code, reason);
    }
}

/// The terminal route's handlers (v2 `LocalTerminalSocketHandlers`). None of
/// them fails: a frame the owner cannot use closes its socket with a reason.
impl LoopbackHandlers for LocalTerminalSockets {
    type Port = LoopbackTerminalPacketPort;

    fn port(&self, socket_id: String, socket: LoopbackSocket) -> Arc<Self::Port> {
        Arc::new(LoopbackTerminalPacketPort::new(
            socket_id,
            socket,
            LOCAL_TERMINAL_MAX_BACKPRESSURE_BYTES,
        ))
    }

    fn on_open(&self, port: &Arc<Self::Port>) -> anyhow::Result<()> {
        LocalTerminalSockets::on_open(self, Arc::clone(port) as Arc<dyn TerminalPacketPort>);
        Ok(())
    }

    fn on_message(&self, port: &Arc<Self::Port>, bytes: Vec<u8>) -> anyhow::Result<()> {
        LocalTerminalSockets::on_message(self, port.as_ref(), &bytes);
        Ok(())
    }

    fn on_close(&self, port: &Arc<Self::Port>) -> anyhow::Result<()> {
        LocalTerminalSockets::on_close(self, port.as_ref());
        Ok(())
    }
}
