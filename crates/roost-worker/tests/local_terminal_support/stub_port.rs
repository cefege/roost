//! The stubbed carrier the `local_terminal_*` suites hand the socket owner,
//! and the frame-case helpers that read what it recorded. Mirrors v2's
//! `StubSocket` / `frameCases` in
//! `apps/worker/tests/local-door/local-terminal-socket.test.ts`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_proto::LocalTerminalServerFrame;
use roost_proto::buffa::Message;
use roost_protocol::terminal_peer::peer::TerminalPeerPacketLane;
use roost_worker::local_terminal::{LocalTerminalSockets, PacketSendResult, TerminalPacketPort};

use super::super::terminal_stream_support::held;
use super::case;

/// A carrier that records every frame it was handed (v2 `StubSocket`).
#[derive(Debug)]
pub struct StubPort {
    socket_id: String,
    pub frames: Mutex<Vec<ServerFrame>>,
    pub close_reasons: Mutex<Vec<String>>,
    pub send_result: Mutex<Option<PacketSendResult>>,
    open: AtomicBool,
    sockets: Weak<LocalTerminalSockets>,
}

impl StubPort {
    /// A loopback carrier that just opened on `sockets`.
    pub fn open_on(sockets: &Arc<LocalTerminalSockets>, socket_id: String) -> Arc<Self> {
        let port = Arc::new(Self {
            socket_id,
            frames: Mutex::new(Vec::new()),
            close_reasons: Mutex::new(Vec::new()),
            send_result: Mutex::new(None),
            open: AtomicBool::new(true),
            sockets: Arc::downgrade(sockets),
        });
        sockets.on_open(Arc::clone(&port) as Arc<dyn TerminalPacketPort>);
        port
    }

    pub fn frames(&self) -> Vec<ServerFrame> {
        held(&self.frames).clone()
    }

    /// The oneof case names, in order, as v2's `frameCases`.
    pub fn cases(&self) -> Vec<&'static str> {
        self.frames().iter().map(case).collect()
    }

    pub fn close_reasons(&self) -> Vec<String> {
        held(&self.close_reasons).clone()
    }

    pub fn last(&self) -> ServerFrame {
        self.frames().pop().expect("a frame was sent")
    }
}

impl TerminalPacketPort for StubPort {
    fn socket_id(&self) -> &str {
        &self.socket_id
    }
    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }
    fn send(&self, bytes: Vec<u8>, _lane: TerminalPeerPacketLane) -> PacketSendResult {
        let frame = LocalTerminalServerFrame::decode_from_slice(&bytes).unwrap();
        held(&self.frames).push(frame.frame.expect("a server frame names its case"));
        held(&self.send_result).unwrap_or(PacketSendResult::Accepted)
    }
    fn close(&self, _code: u16, reason: &str) {
        held(&self.close_reasons).push(reason.to_owned());
        // Mirror the listener: a close always drives on_close.
        if self.open.swap(false, Ordering::SeqCst)
            && let Some(sockets) = self.sockets.upgrade()
        {
            sockets.on_close(self);
        }
    }
}
