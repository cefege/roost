//! The credential the coordinator link dials with, and the honest absence of
//! one. Called by the link loop once per dial, because every dial mints afresh.
//!
//! A credential is never URL material: it travels as the second requested
//! WebSocket subprotocol, and `link_dial::dial_request` is what keeps it that
//! way. This module only produces the string.
//!
//! Minting is re-done per dial rather than cached for the process, because a
//! link that outlives its token authenticates as nothing and fails in a way
//! that reads like a coordinator outage. v2 did the same and refreshed in band
//! on a schedule; the per-dial mint here is strictly simpler and strictly
//! harder to get wrong, at the cost of one signature per reconnect.

/// Why no credential could be produced for a dial.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error("no coordinator credential could be minted: {reason}")]
    Unavailable { reason: String },
}

/// A source of short-lived coordinator credentials.
///
/// `&self` and `Send + Sync` because the link loop mints from inside its own
/// task while a signal handler and the boot sequence are running: the source
/// has to be reachable from all of them without being owned by one.
pub trait CredentialSource: Send + Sync {
    /// A credential for exactly one dial.
    fn mint(&self) -> Result<String, CredentialError>;
}

/// The source the service installs, which cannot mint yet.
///
/// A worker that cannot mint does not dial and does not pretend to. Every
/// attempt is recorded as a non-open dial by the reconnect ladder, so the
/// worker stays a visible, escalating, obviously-unauthenticated daemon rather
/// than a worker that opens a link nobody can trust.
///
/// UNIMPLEMENTED: sign an EdDSA JWT with `aud: "roost-coordinator"` from
/// the worker's OpenSSH ed25519 private key, which is the port of
/// `apps/worker/src/host/jwt.ts`. The pieces that already exist and should be
/// used rather than rewritten: `roost_host::jwt_base::b64url_encode` for the
/// signing input, `roost_protocol::fingerprint::fingerprint_hex` for the
/// registry fingerprint this worker dials as (SHA-256 of the public key), and
/// the key file `WorkerBoot` already resolved. It belongs here rather than at a
/// call site so that every dial goes through it and none of them can skip it.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableCredential;

impl CredentialSource for UnavailableCredential {
    fn mint(&self) -> Result<String, CredentialError> {
        Err(CredentialError::Unavailable {
            reason: "worker key signing is not ported yet, so no JWT can be signed for the dial"
                .to_string(),
        })
    }
}
