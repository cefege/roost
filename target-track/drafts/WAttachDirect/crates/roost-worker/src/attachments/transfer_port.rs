//! The one port contract both direct-attachment carriers implement:
//! `direct_loopback::LoopbackAttachmentTransferPort` and
//! `peer_packet_port::AttachmentPeerPacketPort`. `direct_sockets` owns protobuf
//! admission and destination writes; a carrier owns its bytes, backpressure and
//! close lifecycle. Ports `apps/worker/src/attachments/attachment-transfer-port.ts`.

use std::fmt;

use roost_protocol::attachment_transfer::PeerChannelLane;

/// What a carrier did with one outgoing frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendResult {
    Accepted,
    Backpressured,
    Refused,
}

/// Which carrier a port is, as the transfer log lines name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKind {
    Loopback,
    Webrtc,
}

impl PortKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Webrtc => "webrtc",
        }
    }
}

/// A direct-attachment carrier.
///
/// No method may call back into the handler that owns the port before it
/// returns: `direct_sockets` calls these under its own lock, and a close is
/// reported to it later, from the carrier's own task.
pub trait AttachmentTransferPort: Send + Sync + fmt::Debug {
    fn socket_id(&self) -> &str;
    fn kind(&self) -> PortKind;
    fn is_open(&self) -> bool;
    fn send(&self, bytes: Vec<u8>, lane: PeerChannelLane) -> SendResult;
    fn close(&self, code: Option<u16>, reason: &str);
    /// A WebRTC port waits for its ordered control queue; loopback closes now.
    fn close_after_drain(&self, reason: &str);
    /// WebRTC refuses data-channel chunks until the hello is admitted.
    fn mark_authenticated(&self);
}
