//! The packet port a direct terminal carrier presents to `LocalTerminalSockets`:
//! the loopback WebSocket adapter (`super::loopback`) and the WebRTC peer port
//! (`crate::peer`) implement it; the socket owner chooses the lane and owns the
//! frame encoding, the carrier owns admission, backpressure and its own close.
//! Ports `apps/worker/src/terminal/peer/terminal-packet-port.ts` and the peer
//! port and expected tuple of `local-door/local-terminal-socket{,-controls}.ts`.

use roost_protocol::terminal_peer::peer::TerminalPeerPacketLane;

use crate::uplink::OwnerFuture;

/// What a carrier did with one whole frame. v2 `TerminalPacketSendResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketSendResult {
    /// The carrier took the frame now.
    Accepted,
    /// The carrier queued it within its own bound; the frame is still owned.
    Backpressured,
    /// The carrier cannot take it; nothing was queued.
    Refused,
}

/// One direct carrier, as the socket owner drives it.
pub trait TerminalPacketPort: Send + Sync + std::fmt::Debug {
    /// The id this port is registered, fenced and logged under.
    fn socket_id(&self) -> &str;
    /// False once the carrier has closed, from either side.
    fn is_open(&self) -> bool;
    /// Hand one encoded `LocalTerminalServerFrame` to the carrier on `lane`.
    fn send(&self, bytes: Vec<u8>, lane: TerminalPeerPacketLane) -> PacketSendResult;
    /// Close the carrier. A carrier that reports its own close calls
    /// `LocalTerminalSockets::on_close` (or its `PeerIngress::on_close`), which
    /// is a no-op for a port the owner already retired.
    fn close(&self, code: u16, reason: &str);
}

/// A WebRTC peer port: framing is native, so the owner marks it authenticated
/// once the Hello matched the offer, and history is reserved and drained per
/// lane. v2 `PeerTerminalPacketPort` + `HistoryDrainPort`.
pub trait PeerTerminalPacketPort: TerminalPacketPort {
    /// The Hello matched the offer tuple and its grant: native framing may
    /// admit authenticated traffic.
    fn mark_authenticated(&self);
    /// Reserve room for one history response of up to `bytes`, or refuse.
    fn reserve_history_read(&self, bytes: usize) -> Option<Box<dyn HistoryReadReservation>>;
    /// Resolves once `lane` has drained, and also when the carrier closed.
    fn wait_for_lane_drain(&self, lane: TerminalPeerPacketLane) -> OwnerFuture<()>;
}

/// One reserved history read. Dropping it releases whatever was not
/// transferred to the lane. v2 `TerminalPeerHistoryReadReservation`.
pub trait HistoryReadReservation: Send {
    /// The response is about to be queued: its bytes now belong to the lane.
    fn transfer(&mut self);
}

/// The tuple a peer offer was authorized for, which its Hello must repeat
/// exactly. v2 `TerminalPeerExpectedTuple`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedPeer {
    pub peer_id: String,
    pub grant_id: String,
    pub device_fingerprint: String,
    pub tab_id: String,
    pub worker_epoch: String,
}
