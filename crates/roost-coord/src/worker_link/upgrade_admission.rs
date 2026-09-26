//! Whether a worker WebSocket upgrade is allowed, decided before any connection
//! state exists.
//!
//! Owned by the coordinator's worker link. Pure: it takes the path, the query,
//! the offered subprotocols and the verified caller, and returns a decision. The
//! I/O layer applies it. That split is the point -- the validation ORDER is the
//! security property, and an order that lives in a handler is an order nobody
//! reviews.
//!
//! THE ORDER IS THE CONTRACT (`apps/coord/src/workers/worker-ws-upgrade.ts:37-117`):
//!
//! 1. the path must match `/ws/coord-worker/<64-hex>`;
//! 2. **the entire query string must be empty**;
//! 3. exactly two subprotocols, the marker first and a non-empty credential
//!    second;
//! 4. the credential must verify;
//! 5. the URL fingerprint, the JWT fingerprint, the principal kind, the
//!    principal fingerprint and the key generation must ALL agree.
//!
//! WHY THE WHOLE QUERY IS REJECTED AND NOT JUST A KNOWN PARAMETER. Step 2's
//! comment: "This endpoint has no query contract. Rejecting the entire query
//! surface guarantees an old `?token=` client cannot leak a credential into
//! access logs while still authenticating successfully by subprotocol." A denylist
//! of `token=` would be a denylist of the credential spelling v2 shipped, and the
//! next spelling would be a leak. This is the one place "no query" is a real
//! rule rather than a convention -- note the Sync endpoint legitimately does
//! take a query, so this is per-transport, not global.
//!
//! WHY ALL FIVE OF STEP 5'S CONDITIONS. A valid signature proves a key, not a
//! worker: the same key could be a browser's. The URL is chosen by the dialer,
//! the JWT is minted by the key holder, and the principal is what the database
//! says the key IS. Disagreement between any two means a replay, a stale
//! deployment, or a stolen key being pointed at someone else's machine, and
//! none of the three should get a socket.

use roost_protocol::{ProtocolError, ProtocolResult};

use crate::auth::principal::Principal;

/// The only subprotocol marker a worker may offer, and the only one the
/// coordinator echoes. A proxy that logs the handshake therefore learns that a
/// worker connected and nothing else
/// (`crates/roost-protocol/src/wire/coord_worker.rs:27`).
pub const WORKER_AUTH_SUBPROTOCOL: &str = "roost-worker-auth";

/// The path prefix the worker dials.
pub const WORKER_WS_PATH_PREFIX: &str = "/ws/coord-worker/";

/// The shape a worker upgrade request presents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerUpgradeRequest {
    /// The request path, with no query.
    pub path: String,
    /// The raw query string, including the leading `?` when present. The
    /// admission rule is that this is **empty**, so a caller must pass it
    /// verbatim rather than pre-splitting it.
    pub query: String,
    /// The `sec-websocket-protocol` entries, in the order offered.
    pub offered_protocols: Vec<String>,
    /// The verified caller, or `None` when the credential did not verify.
    pub caller: Option<VerifiedWorkerCaller>,
}

/// The facts a successful credential check established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedWorkerCaller {
    /// The key's fingerprint, from the JWT's `kid`.
    pub fingerprint: String,
    /// The key generation current when the token was verified.
    pub key_generation: u64,
    /// The key's authorized label.
    pub label: String,
}

/// Why an upgrade was refused, and what the caller sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UpgradeRefusal {
    /// Any query string was present. `401 unauthorized`.
    #[error("query_credential")]
    QueryCredential,
    /// The subprotocol envelope was not exactly `[marker, credential]`.
    /// `401 unauthorized`.
    #[error("malformed_subprotocol")]
    MalformedSubprotocol,
    /// The credential did not verify. `401 unauthorized`.
    #[error("jwt_invalid")]
    JwtInvalid,
    /// The URL, the JWT, the principal and the generation did not all agree.
    /// `401 unauthorized`.
    #[error("principal_mismatch")]
    PrincipalMismatch,
}

/// An upgrade decision: either the fingerprint to bind, or a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeDecision {
    /// Admitted, bound to this worker fingerprint.
    Admitted {
        /// The URL fingerprint, which every later step re-checks.
        fingerprint: String,
        /// The verified caller.
        caller: VerifiedWorkerCaller,
    },
    /// Refused. The transport turns this into `401 unauthorized` with a
    /// `signal` carrying the reason; the body never distinguishes the reasons
    /// to the peer, so a prober learns only that it was refused.
    Refused(UpgradeRefusal),
}

impl UpgradeRefusal {
    /// The HTTP status every refusal answers with.
    ///
    /// 401 for all four, deliberately. A `400` for a malformed envelope and a
    /// `401` for a bad signature would tell a prober which half of its guess was
    /// right, and this endpoint is reachable by anything that can open a
    /// socket.
    #[must_use]
    pub const fn status(self) -> u16 {
        401
    }

    /// The body every refusal answers with.
    #[must_use]
    pub const fn body(self) -> &'static str {
        "unauthorized"
    }
}

/// Decide a worker upgrade.
///
/// `principal_is_worker` is the database's answer, already resolved by the
/// caller: this function is about the ORDER of the checks, and handing it a
/// resolved principal keeps the order testable without a database.
#[must_use]
pub fn admit_worker_upgrade(
    request: &WorkerUpgradeRequest,
    principal_is_worker: bool,
    current_generation: Option<u64>,
) -> UpgradeDecision {
    let Some(fingerprint) = parse_path_fingerprint(&request.path) else {
        return UpgradeDecision::Refused(UpgradeRefusal::MalformedSubprotocol);
    };

    // Rule 2, before anything is read out of the header.
    if !request.query.is_empty() {
        return UpgradeDecision::Refused(UpgradeRefusal::QueryCredential);
    }

    // Rule 3.
    if request.offered_protocols.len() != 2
        || request.offered_protocols[0] != WORKER_AUTH_SUBPROTOCOL
        || request.offered_protocols[1].is_empty()
    {
        return UpgradeDecision::Refused(UpgradeRefusal::MalformedSubprotocol);
    }

    // Rule 4.
    let Some(caller) = request.caller.as_ref() else {
        return UpgradeDecision::Refused(UpgradeRefusal::JwtInvalid);
    };

    // Rule 5. All four comparisons, in the order the source evaluates them.
    if caller.fingerprint != fingerprint
        || !principal_is_worker
        || current_generation != Some(caller.key_generation)
    {
        return UpgradeDecision::Refused(UpgradeRefusal::PrincipalMismatch);
    }

    UpgradeDecision::Admitted {
        fingerprint,
        caller: caller.clone(),
    }
}

/// The 64-hex fingerprint in a worker path, if the path has exactly one.
///
/// The hex check is here rather than in a route matcher so a path that is
/// *nearly* right is refused by the same rule as one that is not, with the same
/// answer. A worker whose fingerprint has a stray character must not be able to
/// tell whether the rest of its credential was also examined.
#[must_use]
pub fn parse_path_fingerprint(path: &str) -> Option<String> {
    let fingerprint = path.strip_prefix(WORKER_WS_PATH_PREFIX)?;
    if fingerprint.len() != 64
        || !roost_protocol::fingerprint::is_fingerprint_hex(fingerprint)
        || fingerprint.contains('/')
    {
        return None;
    }
    Some(fingerprint.to_string())
}

/// Re-check that a resolved principal really is the worker this socket claims.
///
/// Called at `open` and again whenever a connection generation is superseded.
/// Returning an error rather than a bool keeps the reason in one place: a
/// browser principal on a worker socket is a routing bug, not a peer fault, and
/// it must not be reported as one.
pub fn require_worker_principal(principal: &Principal, fingerprint: &str) -> ProtocolResult<()> {
    if principal.is_worker() && principal.fingerprint() == fingerprint {
        Ok(())
    } else {
        Err(ProtocolError::new(
            "auth.principal",
            "worker not registered; redeem bootstrap token first",
        ))
    }
}
