//! The vocabulary of terminal-peer signaling: the peer settings, the grant
//! port negotiation reads leases through, the request shape and grant checks,
//! the bounded refusals, and the worker-failure classification. Holds no
//! registry and never retains SDP. Ports
//! `apps/coord/src/terminal/direct/terminal-peer-negotiation-state.ts`.

use std::collections::HashSet;
use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode};
use roost_host::CoordConfig;
use roost_proto::SessionsNegotiateLocalTerminalPeerRequest;
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_MAX_SESSIONS_PER_GRANT, TERMINAL_PEER_SDP_MAX_UTF8_BYTES,
};

use crate::terminal_direct::grant_owner::TerminalGrantOwner;
use crate::terminal_direct::grant_state::{
    AuthorizationFuture, InvalidationListener, TerminalGrantLeaseSnapshot,
};

/// The longest identifier (worker, grant, tab, peer, epoch) a request carries.
pub const TERMINAL_PEER_MAX_IDENTIFIER_UTF8_BYTES: usize = 128;

/// Whether the direct WebRTC carrier is offered, and where it gathers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalPeerSettings {
    /// v2 `cfg.terminalPeerEnabled`.
    pub enabled: bool,
    /// v2 `cfg.terminalPeerStunUrls`.
    pub stun_urls: Vec<String>,
}

impl TerminalPeerSettings {
    /// The settings a resolved config declares. A process with no config
    /// offers no peer, as v2's `deps.cfg?.terminalPeerEnabled === true` reads.
    #[must_use]
    pub fn from_config(config: Option<&CoordConfig>) -> Self {
        config.map_or_else(Self::default, |config| Self {
            enabled: config.terminal_peer_enabled,
            stun_urls: config.terminal_peer_stun_urls.clone(),
        })
    }
}

/// Who is negotiating: the owner key leases are held under, and the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPeerCaller {
    /// v2 `captureOwnerKey(caller)`.
    pub owner_key: String,
    /// The browser device's fingerprint.
    pub device_fingerprint: String,
}

/// Durable route authority over one worker's sessions (v2
/// `authorizeTerminalGrantSessions`), injectable so a test can gate it.
pub type TerminalGrantSessionAuthorizer =
    Arc<dyn Fn(String, Vec<String>) -> AuthorizationFuture + Send + Sync>;

/// The two lease operations signaling needs from the grant owner.
pub trait TerminalPeerGrantPort: Send + Sync {
    /// v2 `ownedGrant`: the exact live lease, or `None`.
    fn owned_grant(
        &self,
        owner_key: &str,
        tab_id: &str,
        worker_fp: &str,
        grant_id: &str,
    ) -> Option<TerminalGrantLeaseSnapshot>;

    /// v2 `subscribeInvalidation`; `None` once the owner is disposed.
    fn subscribe_invalidation(&self, listener: InvalidationListener) -> Option<u64>;

    /// The returned unsubscribe of v2's `subscribeInvalidation`.
    fn unsubscribe_invalidation(&self, id: u64);
}

impl TerminalPeerGrantPort for TerminalGrantOwner {
    fn owned_grant(
        &self,
        owner_key: &str,
        tab_id: &str,
        worker_fp: &str,
        grant_id: &str,
    ) -> Option<TerminalGrantLeaseSnapshot> {
        TerminalGrantOwner::owned_grant(self, owner_key, tab_id, worker_fp, grant_id)
    }

    fn subscribe_invalidation(&self, listener: InvalidationListener) -> Option<u64> {
        TerminalGrantOwner::subscribe_invalidation(self, listener).ok()
    }

    fn unsubscribe_invalidation(&self, id: u64) {
        TerminalGrantOwner::unsubscribe_invalidation(self, id);
    }
}

/// Refuse a request whose identifiers, peer id or offer size are malformed.
pub fn assert_terminal_peer_request_shape(
    request: &SessionsNegotiateLocalTerminalPeerRequest,
) -> Result<(), ConnectError> {
    for (field, value) in [
        ("worker_fp", &request.worker_fp),
        ("grant_id", &request.grant_id),
        ("tab_id", &request.tab_id),
        ("peer_id", &request.peer_id),
        ("worker_epoch", &request.worker_epoch),
    ] {
        if value.is_empty() || value.len() > TERMINAL_PEER_MAX_IDENTIFIER_UTF8_BYTES {
            return Err(terminal_peer_invalid(&format!(
                "terminal peer {field} is invalid"
            )));
        }
    }
    if !is_terminal_peer_uuid(&request.peer_id) {
        return Err(terminal_peer_invalid("terminal peer peer_id is invalid"));
    }
    if request.offer_sdp.len() > TERMINAL_PEER_SDP_MAX_UTF8_BYTES {
        return Err(terminal_peer_invalid("terminal peer offer is invalid"));
    }
    Ok(())
}

/// Whether a lease's session list is one a peer may be negotiated over.
#[must_use]
pub fn has_valid_terminal_peer_grant_sessions(session_ids: &[String]) -> bool {
    let unique: HashSet<&str> = session_ids.iter().map(String::as_str).collect();
    !session_ids.is_empty()
        && session_ids.len() <= TERMINAL_PEER_MAX_SESSIONS_PER_GRANT
        && unique.len() == session_ids.len()
        && session_ids.iter().all(|session| {
            !session.is_empty() && session.len() <= TERMINAL_PEER_MAX_IDENTIFIER_UTF8_BYTES
        })
}

/// A versioned (1-8), RFC 4122 variant uuid, either case (v2 `TERMINAL_PEER_UUID`).
fn is_terminal_peer_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => (b'1'..=b'8').contains(byte),
            19 => matches!(byte.to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_hexdigit(),
        })
}

/// The owner/tab/worker tuple one admission and one pending offer live under.
pub(crate) type PeerKey = (String, String, String);

/// `InvalidArgument`, with v2's wording.
pub(crate) fn terminal_peer_invalid(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::InvalidArgument, message)
}

/// `PermissionDenied`, with v2's wording.
pub(crate) fn terminal_peer_denied(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::PermissionDenied, message)
}

/// `Unavailable`, with v2's wording.
pub(crate) fn terminal_peer_unavailable(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::Unavailable, message)
}

/// `ResourceExhausted`, with v2's wording.
pub(crate) fn terminal_peer_exhausted(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::ResourceExhausted, message)
}

/// `AlreadyExists`, with v2's wording.
pub(crate) fn terminal_peer_already_exists(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::AlreadyExists, message)
}

/// `Canceled`, with v2's wording.
pub(crate) fn terminal_peer_cancelled() -> ConnectError {
    ConnectError::new(ErrorCode::Canceled, "terminal peer negotiation cancelled")
}

/// `DeadlineExceeded`, with v2's wording.
pub(crate) fn terminal_peer_deadline_exceeded() -> ConnectError {
    ConnectError::new(
        ErrorCode::DeadlineExceeded,
        "terminal peer answer timed out",
    )
}

/// What a worker's refusal becomes for the browser: a grant problem is the
/// browser's to fix, capacity is retryable later, anything else is transient.
pub(crate) fn terminal_peer_worker_failure(reason: &str) -> ConnectError {
    let message = format!("terminal peer worker rejected negotiation: {reason}");
    match reason {
        "grant_unavailable" | "expired" => terminal_peer_denied(&message),
        "capacity" => terminal_peer_exhausted(&message),
        _ => terminal_peer_unavailable(&message),
    }
}
