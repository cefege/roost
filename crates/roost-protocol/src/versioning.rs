//! Every wire literal that names a version, a capability, or a channel.
//!
//! One constant per row of the contract's versioning table in
//! `protocol/README.md`, so a label is spelled exactly once in the whole
//! workspace. A capability string that is written out in two places drifts
//! the moment one of them is edited, and the symptom — a peer that silently
//! negotiates a capability it does not implement — is invisible until a
//! feature stops working on one machine.

/// Worker capability for the direct terminal WebRTC carrier.
pub const CAPABILITY_TERMINAL_PEER_WEBRTC_V1: &str = "terminal-peer-webrtc-v1";

/// Worker capability for acknowledged direct-terminal input-route handoff.
pub const CAPABILITY_TERMINAL_INPUT_ROUTE_V1: &str = "terminal-input-route-v1";

/// Worker capability for the attachment-specific WebRTC carrier.
pub const CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1: &str =
    "attachment-transfer-peer-webrtc-v1";

/// Worker capability for coordinator-owned terminal view authority.
pub const CAPABILITY_TERMINAL_VIEW_OWNER_V1: &str = "terminal-view-owner-v1";

/// Worker capability for terminal metadata on the worker link.
pub const CAPABILITY_TERMINAL_METADATA_V1: &str = "terminal-metadata-v1";

/// Direct-terminal WebRTC control-channel label.
pub const CHANNEL_TERMINAL_CONTROL_V1: &str = "roost-terminal-control-v1";

/// Direct-terminal WebRTC terminal-lane label.
pub const CHANNEL_TERMINAL_DATA_V1: &str = "roost-terminal-data-v1";

/// Direct-terminal WebRTC history-lane label.
pub const CHANNEL_TERMINAL_HISTORY_V1: &str = "roost-terminal-history-v1";

/// Ordered attachment WebRTC control-channel label.
pub const CHANNEL_ATTACHMENT_CONTROL_V1: &str = "roost-attachment-control-v1";

/// Ordered attachment WebRTC data-channel label.
pub const CHANNEL_ATTACHMENT_DATA_V1: &str = "roost-attachment-data-v1";

/// Worker loopback WebSocket subprotocol for attachment transfer.
pub const SUBPROTOCOL_LOCAL_ATTACHMENT_TRANSFER_V1: &str = "roost-local-attachment-transfer-v1";

/// The envelope key that carries an explicit schema discriminator on a
/// versioned non-protobuf payload. Not a value: a key name, which is why it
/// belongs here with the other literals rather than in one payload's module.
pub const KEY_SCHEMA_VERSION: &str = "schema_version";

/// The envelope key that carries the keeper contract's compatibility
/// discriminator.
pub const KEY_PROTOCOL_VERSION: &str = "protocol_version";

/// The Sync WebSocket query value that selects domain generations and socket
/// identity. A string because it travels in a URL query.
pub const SYNC_QUERY_V2: &str = "2";
