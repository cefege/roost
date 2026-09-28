//! The direct-attachment carrier contract browser, coordinator and worker all
//! read: the loopback route, chunk and packet bounds, deadlines, the two peer
//! data channels, and both refusal vocabularies. Ports
//! `packages/protocol/src/attachment-transfer.ts`; the peer framing is in
//! `packets` and its send queue in `packet_queue`. Called by the worker's
//! `attachments` carriers and `door`.

pub mod packet_queue;
pub mod packets;

use crate::versioning::{
    CHANNEL_ATTACHMENT_CONTROL_V1, CHANNEL_ATTACHMENT_DATA_V1,
    SUBPROTOCOL_LOCAL_ATTACHMENT_TRANSFER_V1,
};

pub use packet_queue::{AttachmentTransferPacketQueue, AttachmentTransferPacketQueueFragment};
pub use packets::{
    AttachmentTransferPacket, AttachmentTransferPacketAssembler, AttachmentTransferPacketDirection,
    AttachmentTransferPacketError, AttachmentTransferPacketHeader, AttachmentTransferPacketQuota,
    encode_attachment_transfer_packet, parse_attachment_transfer_packet,
};

pub const LOOPBACK_PATH: &str = "/ws/local-attachment-transfer";
pub const LOOPBACK_SUBPROTOCOL: &str = SUBPROTOCOL_LOCAL_ATTACHMENT_TRANSFER_V1;
pub const DIRECT_CHUNK_BYTES: usize = 512 * 1024;
pub const CHUNK_SHA256_HEX_LENGTH: usize = 64;
pub const LOOPBACK_MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const PACKET_MAGIC: u32 = 0x3150_5441;
pub const PACKET_VERSION: u32 = 1;
pub const PACKET_HEADER_BYTES: usize = 20;
pub const PACKET_MAX_BYTES: usize = 16_384;
pub const PACKET_MAX_PAYLOAD_BYTES: usize = PACKET_MAX_BYTES - PACKET_HEADER_BYTES;
pub const PACKET_LOGICAL_FRAME_MAX_BYTES: usize = 1024 * 1024;
pub const PACKET_STALL_MS: u64 = 10_000;
pub const MAX_CHUNKS_IN_FLIGHT: usize = 1;
pub const MAX_ACTIVE_PER_WORKER: usize = 8;
pub const MAX_ACTIVE_PER_BROWSER_DOCUMENT: usize = 8;
pub const ACTIVE_MAX_MS: u64 = 12 * 60 * 60_000;
pub const IDLE_MS: u64 = 5 * 60_000;
pub const GRANT_TTL_MS: u64 = 60_000;
pub const GRANT_ACK_DEADLINE_MS: u64 = 8_000;
pub const MAX_PENDING_GRANTS: usize = 64;
pub const MAX_PENDING_GRANTS_PER_DEVICE: usize = 8;
pub const MAX_PENDING_STATUS_REQUESTS: usize = 64;

pub const PEER_MAX_NEGOTIATIONS_PER_WORKER: usize = 4;
pub const PEER_MAX_PENDING_NEGOTIATIONS: usize = 64;
pub const PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE: usize = 8;
pub const PEER_NEGOTIATION_DEADLINE_MS: u64 = 15_000;
pub const PEER_ICE_GATHERING_DEADLINE_MS: u64 = 3_000;
pub const PEER_NATIVE_ANSWER_DEADLINE_MS: u64 = 8_000;
pub const HELLO_DEADLINE_MS: u64 = 3_000;
pub const ACK_DEADLINE_MS: u64 = 15_000;
pub const STATUS_DEADLINE_MS: u64 = 8_000;

pub const PEER_CONTROL_QUEUE_MAX_BYTES: usize = 128 * 1024;
pub const PEER_DATA_QUEUE_MAX_BYTES: usize = 1024 * 1024;
pub const PEER_WORKER_DATA_QUEUE_MAX_BYTES: usize = 32 * 1024 * 1024;

pub const PEER_DATA_CHANNEL_PROTOCOL: &str = "roost.attachment-transfer.v1";

/// The reason a direct carrier closes after the final acknowledgement.
pub const COMPLETE_REASON: &str = "complete";

/// Which of the two attachment data channels a message travels on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeerChannelLane {
    Control,
    Data,
}

impl PeerChannelLane {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Data => "data",
        }
    }
}

/// One negotiated attachment data channel. Both ends create it with this id,
/// so the far end can never choose which channel this side believes is control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerDataChannelDefinition {
    pub lane: PeerChannelLane,
    pub id: u16,
    pub label: &'static str,
    pub ordered: bool,
    pub protocol: &'static str,
}

pub const PEER_DATA_CHANNELS: [PeerDataChannelDefinition; 2] = [
    PeerDataChannelDefinition {
        lane: PeerChannelLane::Control,
        id: 0,
        label: CHANNEL_ATTACHMENT_CONTROL_V1,
        ordered: true,
        protocol: PEER_DATA_CHANNEL_PROTOCOL,
    },
    PeerDataChannelDefinition {
        lane: PeerChannelLane::Data,
        id: 1,
        label: CHANNEL_ATTACHMENT_DATA_V1,
        ordered: true,
        protocol: PEER_DATA_CHANNEL_PROTOCOL,
    },
];

/// Why an attachment peer offer was refused (`WLocalAttachmentPeerError.reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerErrorReason {
    Disabled,
    NativeUnavailable,
    InvalidOffer,
    GrantUnavailable,
    Capacity,
    Expired,
    ConnectionSuperseded,
    IceFailed,
}

impl PeerErrorReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NativeUnavailable => "native_unavailable",
            Self::InvalidOffer => "invalid_offer",
            Self::GrantUnavailable => "grant_unavailable",
            Self::Capacity => "capacity",
            Self::Expired => "expired",
            Self::ConnectionSuperseded => "connection_superseded",
            Self::IceFailed => "ice_failed",
        }
    }
}

/// Why a direct upload was refused (`AttachmentTransferAck.error` and
/// `AttachmentTransferClosed.reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferErrorReason {
    InvalidHello,
    GrantUnavailable,
    UploadNotFound,
    UploadMismatch,
    ChunkOutOfOrder,
    ChunkOffsetMismatch,
    ChunkSha256Mismatch,
    ChunkTooLarge,
    TotalBytesMismatch,
    WriteFailed,
}

impl TransferErrorReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidHello => "invalid_hello",
            Self::GrantUnavailable => "grant_unavailable",
            Self::UploadNotFound => "upload_not_found",
            Self::UploadMismatch => "upload_mismatch",
            Self::ChunkOutOfOrder => "chunk_out_of_order",
            Self::ChunkOffsetMismatch => "chunk_offset_mismatch",
            Self::ChunkSha256Mismatch => "chunk_sha256_mismatch",
            Self::ChunkTooLarge => "chunk_too_large",
            Self::TotalBytesMismatch => "total_bytes_mismatch",
            Self::WriteFailed => "write_failed",
        }
    }
}

/// The lowercase hexadecimal SHA-256 digest a direct chunk echoes.
pub fn is_chunk_sha256(value: &str) -> bool {
    value.len() == CHUNK_SHA256_HEX_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
