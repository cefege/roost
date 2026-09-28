//! The direct attachment-transfer bounds and vocabularies the coordinator
//! enforces: grant lifetimes and capacities, the peer-signaling and receipt
//! deadlines, and the worker's fixed error reasons.
//! Read by `attachments::{grant, grant_state, peer, peer_state, status_results,
//! rpc_direct}`. Ports the coordinator's subset of
//! `packages/protocol/src/attachment-transfer.ts` (the worker keeps its own copy).

/// How long a minted grant authorizes its one upload.
pub const ATTACHMENT_TRANSFER_GRANT_TTL_MS: u32 = 60_000;
/// How long the coordinator waits for a worker to acknowledge a grant install.
pub const ATTACHMENT_TRANSFER_GRANT_ACK_DEADLINE_MS: u64 = 8_000;
/// Live plus minting grants one worker may hold.
pub const ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER: usize = 8;
/// Live plus minting grants one browser document (owner and tab) may hold.
pub const ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_BROWSER_DOCUMENT: usize = 8;
/// Grants minting at once across the process.
pub const ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS: usize = 64;
/// Grants one device may be minting at once.
pub const ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS_PER_DEVICE: usize = 8;
/// Receipt-status requests in flight across the process.
pub const ATTACHMENT_TRANSFER_MAX_PENDING_STATUS_REQUESTS: usize = 64;
/// How long a receipt-status request waits for its worker.
pub const ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS: u64 = 8_000;
/// Peer negotiations admitted or pending against one worker.
pub const ATTACHMENT_TRANSFER_PEER_MAX_NEGOTIATIONS_PER_WORKER: usize = 4;
/// Peer negotiations admitted or pending across the process.
pub const ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS: usize = 64;
/// Peer negotiations one device may have admitted or pending.
pub const ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE: usize = 8;
/// How long an offer waits for the worker's native answer.
pub const ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS: u64 = 8_000;

/// The only reasons a worker may refuse an attachment peer offer with.
pub const ATTACHMENT_TRANSFER_PEER_ERROR_REASONS: [&str; 8] = [
    "disabled",
    "native_unavailable",
    "invalid_offer",
    "grant_unavailable",
    "capacity",
    "expired",
    "connection_superseded",
    "ice_failed",
];

/// The only transfer failures a durable receipt may name.
pub const ATTACHMENT_TRANSFER_ERROR_REASONS: [&str; 10] = [
    "invalid_hello",
    "grant_unavailable",
    "upload_not_found",
    "upload_mismatch",
    "chunk_out_of_order",
    "chunk_offset_mismatch",
    "chunk_sha256_mismatch",
    "chunk_too_large",
    "total_bytes_mismatch",
    "write_failed",
];

/// The largest integer a v2 browser holds exactly (`Number.MAX_SAFE_INTEGER`);
/// a byte count past it cannot round-trip through the web client.
pub const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// A lowercase hexadecimal SHA-256 digest, as direct chunks echo it.
#[must_use]
pub fn is_attachment_transfer_chunk_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether an upload id is non-empty, bounded, and names no path or control
/// character (v2's `/[\\/\x00-\x1f\x7f]/u` refusal, shared by the grant, the
/// status RPC and the status owner).
#[must_use]
pub fn is_opaque_upload_id(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && !value
            .chars()
            .any(|character| matches!(character, '\\' | '/' | '\u{0}'..='\u{1f}' | '\u{7f}'))
}

/// A non-empty identifier within `max_bytes` UTF-8 bytes (v2 `hasAtMostUtf8Bytes`).
#[must_use]
pub fn is_bounded_identifier(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes
}
