//! The public shape of one local terminal grant, what a change to it reports,
//! and the checks an install and a Hello run. `super::grants` is the store that
//! holds them; `super::sockets` and the peer owner read only this public scope,
//! never a digest or a secret. Ports the types and validators of
//! `apps/worker/src/local-door/local-terminal-grants.ts` and
//! `verifyLocalEndpointCapability` (`packages/host/src/local-endpoint.ts`).

use std::sync::Arc;

use roost_protocol::terminal_peer::peer::TERMINAL_PEER_MAX_SESSIONS_PER_GRANT;
use sha2::{Digest, Sha256};
use tokio::time::Instant;

/// The most grants one worker holds at once.
pub const MAX_GRANTS: usize = 256;
/// The longest a coordinator may authorize one grant for.
pub const MAX_TTL_MS: u32 = 24 * 60 * 60_000;
const MAX_ID_BYTES: usize = 128;

/// One coordinator-authorized scope, without its credential digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTerminalGrant {
    pub grant_id: String,
    /// Unique, in install order.
    pub session_ids: Vec<String>,
    pub device_fingerprint: String,
    pub tab_id: String,
    /// Empty for a grant from a coordinator that does not fence epochs; such a
    /// grant is loopback-only.
    pub worker_epoch: String,
    pub expires_at: Instant,
}

/// What a Hello presents. v2 `LocalTerminalCredential`.
#[derive(Debug, Clone, Copy)]
pub struct GrantCredential<'a> {
    pub grant_id: &'a str,
    pub secret: &'a str,
    pub tab_id: &'a str,
    pub device_fingerprint: &'a str,
}

/// Why a grant left the store. The socket owner reads `Expired` apart from the
/// rest to name the close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantRemovalReason {
    Expired,
    Revoked,
    Disposed,
}

impl GrantRemovalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::Revoked => "revoked",
            Self::Disposed => "disposed",
        }
    }
}

/// One change a subscriber is told about, after the store's lock is released.
#[derive(Debug, Clone)]
pub enum GrantChange {
    Installed {
        grant: Arc<LocalTerminalGrant>,
    },
    /// A renewal of a live grant id; `removed_session_ids` is what the new scope
    /// no longer covers, so a carrier holding one of them is closed.
    Renewed {
        grant: Arc<LocalTerminalGrant>,
        removed_session_ids: Vec<String>,
    },
    Removed {
        grant: Arc<LocalTerminalGrant>,
        reason: GrantRemovalReason,
    },
}

/// A peer offer's grant check: the tuple against the live scope and an exact
/// worker epoch. Read by the terminal peer owner (`crate::peer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerGrantAuthorization {
    Authorized,
    GrantUnavailable,
    Expired,
}

/// A grant id, device fingerprint, tab id or epoch: present and bounded.
pub(super) fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ID_BYTES
}

/// The session ids an install names, deduplicated in order, or `None` when the
/// set is empty, too large, or holds an invalid id.
pub(super) fn valid_session_ids(values: &[String]) -> Option<Vec<String>> {
    if values.is_empty() || values.len() > TERMINAL_PEER_MAX_SESSIONS_PER_GRANT {
        return None;
    }
    let mut unique: Vec<String> = Vec::with_capacity(values.len());
    for value in values {
        if !valid_id(value) {
            return None;
        }
        if !unique.contains(value) {
            unique.push(value.clone());
        }
    }
    Some(unique)
}

/// A lowercase hex SHA-256 digest, as the coordinator installs one.
pub(super) fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// The lowercase hex SHA-256 of a presented secret.
pub(super) fn sha256_hex(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    hex
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// v2 `verifyLocalEndpointCapability`: both values are hashed to a fixed width
/// and compared without an early exit, so the comparison time says nothing
/// about how much of the installed digest a guess matched.
pub(super) fn capability_matches(expected: &str, received: &str) -> bool {
    let expected = Sha256::digest(expected.as_bytes());
    let received = Sha256::digest(received.as_bytes());
    expected
        .iter()
        .zip(received.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}
