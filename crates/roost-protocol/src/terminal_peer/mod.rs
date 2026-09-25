//! The direct terminal carrier: capability labels, the fragment framing, the
//! fragment queue, and SDP inspection.
//!
//! `packets` owns the only binary header in the direct path. Every byte a
//! browser receives for terminal data passes through its parser.

pub mod packet_queue;
pub mod packets;
pub mod peer;
pub mod sdp;

pub use packet_queue::TerminalPeerPacketQueue;
pub use packets::{
    TERMINAL_PEER_PACKET_HEADER_BYTES, TerminalPeerPacketAssembler, TerminalPeerPacketError,
    TerminalPeerPacketHeader, encode_terminal_peer_packet, parse_terminal_peer_packet,
};
pub use peer::{
    TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES, TERMINAL_PEER_PACKET_MAGIC,
    TERMINAL_PEER_PACKET_MAX_BYTES, TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
};
