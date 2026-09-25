//! Every limit, identity, and channel label of the direct terminal carrier.
//!
//! Coordinator, worker, and browser read these from here rather than
//! restating them, because a limit written in two places stops being a limit
//! the moment one copy is edited. The capability and channel-label strings
//! live in `crate::versioning`; this module holds the numbers around them, and
//! `stun_url` holds the one operator-supplied value that names a server.

mod stun_url;

pub use crate::versioning::{
    CAPABILITY_TERMINAL_INPUT_ROUTE_V1, CAPABILITY_TERMINAL_PEER_WEBRTC_V1,
};
pub(crate) use stun_url::is_canonical_decimal;
pub use stun_url::parse_terminal_peer_stun_urls;

// A worker that negotiates without bound starves its established peers of
// descriptors, and one browser tab that opens peers without bound is a fleet member.
pub const TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER: usize = 32;
pub const TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER: usize = 4;
pub const TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT: usize = 8;
pub const TERMINAL_PEER_MAX_SESSIONS_PER_GRANT: usize = 256;
pub const TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS: usize = 64;
pub const TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE: usize = 8;

// A peer that misses a deadline is retired rather than waited on, so a
// half-open negotiation cannot hold a slot for the life of the connection.
pub const TERMINAL_PEER_NEGOTIATION_DEADLINE_MS: u64 = 15_000;
pub const TERMINAL_PEER_ICE_GATHERING_DEADLINE_MS: u64 = 3_000;
pub const TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS: u64 = 8_000;
pub const TERMINAL_PEER_HELLO_DEADLINE_MS: u64 = 3_000;
pub const TERMINAL_PEER_HEARTBEAT_INTERVAL_MS: u64 = 5_000;
pub const TERMINAL_PEER_PROBE_DEADLINE_MS: u64 = 3_000;

// A peer is only live once it has answered a probe within one heartbeat
// interval plus one probe deadline; slower than that, the relay is better.
pub const TERMINAL_PEER_PROBE_QUALIFICATION_MS: u64 =
    TERMINAL_PEER_HEARTBEAT_INTERVAL_MS + TERMINAL_PEER_PROBE_DEADLINE_MS;

// A fragment that has not advanced for this long is a dead sender: the
// assembler releases the partial and the caller retires the peer.
pub const TERMINAL_PEER_PACKET_STALL_MS: u64 = 10_000;

// SDP admission bounds, refused before a single line is parsed.
pub const TERMINAL_PEER_SDP_MAX_UTF8_BYTES: usize = 64 * 1024;
pub const TERMINAL_PEER_SDP_MAX_LINES: usize = 512;
pub const TERMINAL_PEER_SDP_MAX_LINE_UTF8_BYTES: usize = 4 * 1024;
pub const TERMINAL_PEER_SDP_MAX_CANDIDATES: usize = 64;
pub const TERMINAL_PEER_SDP_ICE_UFRAG_MAX_UTF8_BYTES: usize = 256;
pub const TERMINAL_PEER_SDP_ICE_PASSWORD_MIN_UTF8_BYTES: usize = 22;
pub const TERMINAL_PEER_SDP_ICE_PASSWORD_MAX_UTF8_BYTES: usize = 256;

// One packet on the wire, header included, and so its SCTP message ceiling.
pub const TERMINAL_PEER_MAX_MESSAGE_SIZE: usize = 16 * 1024;

// The `max-message-size` assumed when a peer advertises none, and the smallest
// this contract accepts: less than one packet cannot be carried.
pub const TERMINAL_PEER_SDP_DEFAULT_MAX_MESSAGE_SIZE: u64 = 64 * 1024;

// The wire bytes `52 54 50 31` — ASCII `RTP1` — so a misrouted packet is
// recognisable before any field in it is trusted.
pub const TERMINAL_PEER_PACKET_MAGIC: u32 = 0x3150_5452;

// Magic, message id, total logical bytes, fragment offset: four little-endian
// `u32`s and nothing else. There is no length field — the fragment is the rest
// of the packet, and completeness is `offset + payload <= total`.
pub const TERMINAL_PEER_PACKET_HEADER_BYTES: usize = 16;

pub const TERMINAL_PEER_PACKET_MAX_BYTES: usize = TERMINAL_PEER_MAX_MESSAGE_SIZE;
pub const TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES: usize =
    TERMINAL_PEER_PACKET_MAX_BYTES - TERMINAL_PEER_PACKET_HEADER_BYTES;

pub const TERMINAL_PEER_LANE_PRIORITY: [TerminalPeerPacketLane; 3] = TerminalPeerPacketLane::ALL;

// Per-lane logical message caps. They are per lane because the lanes are not
// interchangeable: a peer must not spend the history budget on terminal output.
pub const TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES: TerminalPeerLaneByteCaps =
    TerminalPeerLaneByteCaps;

#[derive(Debug)]
pub struct TerminalPeerLaneByteCaps;

impl TerminalPeerLaneByteCaps {
    pub const CONTROL: usize = 128 * 1024;
    pub const TERMINAL: usize = 2 * 1024 * 1024;
    pub const HISTORY: usize = 64 * 1024 * 1024;

    pub fn for_lane(lane: TerminalPeerPacketLane) -> usize {
        [Self::CONTROL, Self::TERMINAL, Self::HISTORY][lane as usize]
    }
}

// These are the values a lane's `TerminalPeerPacketQuota` is configured with,
// which is why they are caps and not fields: the queue asks its owner how much
// it may retain rather than deciding for itself.
pub const TERMINAL_PEER_UNAUTHENTICATED_CONTROL_MAX_BYTES: usize = 4 * 1024;
pub const TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES: usize = 256 * 1024;
pub const TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES: usize = 64 * 1024 * 1024;
pub const TERMINAL_PEER_WORKER_APPLICATION_QUEUE_MAX_BYTES: usize = 128 * 1024 * 1024;
pub const TERMINAL_PEER_WORKER_CONTROL_QUEUE_MAX_BYTES: usize = 32 * 256 * 1024;
pub const TERMINAL_PEER_WORKER_QUEUE_MAX_BYTES: usize = 136 * 1024 * 1024;

// How much one turn of the event loop may push, and how many history reads a
// peer may make, so a slow peer stalls on backpressure.
pub const TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN: usize = 64 * 1024;
pub const TERMINAL_PEER_MAX_HISTORY_READS_PER_PEER: usize = 1;

// SCTP transport bounds for one carrier. The chunk cap is the count bound on a
// lane's send path, and it belongs to the transport, not to the logical queue:
// a queued message is fragmented before it reaches SCTP.
pub const TERMINAL_PEER_SCTP_SEND_BUFFER_BYTES: usize = 256 * 1024;
pub const TERMINAL_PEER_SCTP_RECEIVE_BUFFER_BYTES: usize = 512 * 1024;
pub const TERMINAL_PEER_SCTP_MAX_CHUNKS_ON_QUEUE: usize = 2_048;

// The per-lane `(high, low)` byte watermarks: the retained count that suspends
// a backpressured lane, and the much lower one it resumes at.
pub const TERMINAL_PEER_CHANNEL_WATERMARKS: TerminalPeerChannelWatermarks =
    TerminalPeerChannelWatermarks;

#[derive(Debug)]
pub struct TerminalPeerChannelWatermarks;

impl TerminalPeerChannelWatermarks {
    pub const CONTROL: (usize, usize) = (32 * 1024, 8 * 1024);
    pub const TERMINAL: (usize, usize) = (64 * 1024, 16 * 1024);
    pub const HISTORY: (usize, usize) = (16 * 1024, 4 * 1024);

    pub fn for_lane(lane: TerminalPeerPacketLane) -> (usize, usize) {
        [Self::CONTROL, Self::TERMINAL, Self::HISTORY][lane as usize]
    }
}

pub const TERMINAL_PEER_DATA_CHANNEL_PROTOCOL: &str = "roost.local-terminal.v1";

// The negotiated channels, in stream-identity order: control 0, terminal 1,
// history 2. These identities are contract, not a local choice.
pub const TERMINAL_PEER_DATA_CHANNELS: [TerminalPeerDataChannelDefinition; 3] = [
    TerminalPeerDataChannelDefinition {
        lane: TerminalPeerPacketLane::Control,
        id: 0,
        label: crate::versioning::CHANNEL_TERMINAL_CONTROL_V1,
        protocol: TERMINAL_PEER_DATA_CHANNEL_PROTOCOL,
    },
    TerminalPeerDataChannelDefinition {
        lane: TerminalPeerPacketLane::Terminal,
        id: 1,
        label: crate::versioning::CHANNEL_TERMINAL_DATA_V1,
        protocol: TERMINAL_PEER_DATA_CHANNEL_PROTOCOL,
    },
    TerminalPeerDataChannelDefinition {
        lane: TerminalPeerPacketLane::History,
        id: 2,
        label: crate::versioning::CHANNEL_TERMINAL_HISTORY_V1,
        protocol: TERMINAL_PEER_DATA_CHANNEL_PROTOCOL,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalPeerDataChannelDefinition {
    pub lane: TerminalPeerPacketLane,
    pub id: u8,
    pub label: &'static str,
    pub protocol: &'static str,
}

// Acknowledged-input work bounds. The per-port caps exist because one port
// with a stalled consumer must not spend the peer's whole retained budget.
pub const TERMINAL_PEER_INPUT_WORK_MAX_OUTSTANDING: usize = 256;
pub const TERMINAL_PEER_INPUT_WORK_MAX_RETAINED_BYTES: usize = 16 * 1024 * 1024;
pub const TERMINAL_PEER_INPUT_WORK_MAX_OUTSTANDING_PER_PORT: usize = 32;
pub const TERMINAL_PEER_INPUT_WORK_MAX_RETAINED_BYTES_PER_PORT: usize = 2 * 1024 * 1024;

// Input-route claim bounds. One claim per actor session, so a reconnecting
// actor cannot race its own predecessor into the route.
pub const TERMINAL_PEER_ROUTE_CLAIM_MAX_OUTSTANDING: usize = 32;
pub const TERMINAL_PEER_ROUTE_CLAIM_MAX_OUTSTANDING_PER_PORT: usize = 4;
pub const TERMINAL_PEER_ROUTE_CLAIM_MAX_OUTSTANDING_PER_ACTOR_SESSION: usize = 1;

/// One ordered data channel's logical traffic. The lane is chosen by the owner
/// of the channel, never parsed out of a packet: the header carries no lane
/// byte, so a peer cannot reach another lane's state by relabelling its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
// `repr(usize)` pins the discriminants, so a lane indexes the per-lane tables
// above instead of a hand-written `match` that could fall out of step.
#[repr(usize)]
pub enum TerminalPeerPacketLane {
    Control,
    Terminal,
    History,
}

impl TerminalPeerPacketLane {
    /// Every lane, in the priority order a sender drains them: control first,
    /// then live terminal output, then history backfill.
    pub const ALL: [Self; 3] = [Self::Control, Self::Terminal, Self::History];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Terminal => "terminal",
            Self::History => "history",
        }
    }
}

pub const TERMINAL_PEER_STUN_URLS_ENV: &str = "ROOST_TERMINAL_PEER_STUN_URLS";

// The account-free server both peers gather through when the operator declares
// none, so an unconfigured install still gets a reflexive candidate.
pub const DEFAULT_TERMINAL_PEER_STUN_URLS: [&str; 1] = ["stun:stun.cloudflare.com:3478"];

// At most this many operator STUN servers: the list is shared with the browser
// verbatim, so it is bounded before it is parsed.
pub const TERMINAL_PEER_STUN_URL_MAX_COUNT: usize = 4;
