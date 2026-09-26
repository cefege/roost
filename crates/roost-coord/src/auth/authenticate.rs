//! Turning a presented bearer into a principal, for whichever transport
//! presented it.
//!
//! Owned by the coordinator's auth layer and called by all three surfaces that
//! accept a credential: the Connect interceptor, the worker upgrade and the Sync
//! upgrade. One implementation for all three, because "what does this token
//! prove" has one answer, and three implementations is how a fleet ends up with
//! a worker socket that accepts a browser key.
//!
//! THE FOUR GENERATION CHECKS LIVE INSIDE, NOT AROUND, THIS FUNCTION. A caller
//! that verified the signature itself and then asked for a principal would have
//! nowhere to put them, and the revocation race in
//! `apps/coord/src/auth/jwt.ts:108,117,181,222` would reopen.

use crate::auth::authorized_keys::{load_authorized_key, public_key_of, resolve_key_principal};
use crate::auth::jwt_crypto::PublicKey;
use crate::auth::jwt_key_cache::JwtKeyCache;
use crate::auth::jwt_verify::{VerifyClock, VerifyContext, verify_token};
use crate::auth::principal::Principal;
use crate::db::CoordDb;

/// A credential that did not verify, and why.
///
/// Carries the key identity separately from the failure so the transports can
/// `signal` a precise reason: an unknown `kid` is a stale client, `signature
/// invalid` is the only value a peer can influence, and a signal that cannot tell
/// them apart is a signal nobody can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthFailure {
    /// No credential was presented.
    #[error("no credential")]
    Absent,
    /// The credential was not a well-formed compact JWS, or its header or
    /// claims were unusable.
    #[error("{0}")]
    Malformed(String),
    /// No authorized key is registered under the presented `kid`.
    #[error("unknown kid {kid}")]
    UnknownKid {
        /// The `kid` the token named.
        kid: String,
    },
    /// The stored row is not a usable 32-byte public key: a database fault, not
    /// a peer fault.
    #[error("stored key: {0}")]
    StoredKey(String),
    /// The signature, the time bounds or the generation refused it.
    #[error("{0}")]
    Verify(String),
    /// The key verified but resolves to no principal right now.
    #[error("no principal for {kid}")]
    NoPrincipal {
        /// The `kid` the token named.
        kid: String,
    },
    /// A database read failed. The only failure here that is the coordinator's
    /// fault rather than the peer's.
    #[error("sqlite: {0}")]
    Database(String),
}

impl AuthFailure {
    /// Whether this failure is a stale client rather than a hostile one.
    ///
    /// A stale client is one whose key is gone; a hostile one presents a token
    /// that verifies against a key it should not be able to sign for. Only the
    /// second is worth paging someone, and the two must not share a signal.
    #[must_use]
    pub fn is_absent_key(&self) -> bool {
        matches!(
            self,
            AuthFailure::Absent | AuthFailure::UnknownKid { .. } | AuthFailure::NoPrincipal { .. }
        )
    }

    /// Whether this failure is the coordinator's fault.
    #[must_use]
    pub fn is_coordinator_fault(&self) -> bool {
        matches!(self, AuthFailure::Database(_) | AuthFailure::StoredKey(_))
    }
}

/// A credential that verified, and the principal it proves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedCaller {
    /// The key's fingerprint.
    pub fingerprint: String,
    /// The key's authorized label.
    pub label: String,
    /// What the key is authorized as.
    pub principal: Principal,
    /// The key generation current when the token verified. A long-lived socket
    /// re-checks it, so a revocation mid-socket still closes the socket.
    pub key_generation: u64,
    /// When this credential stops being acceptable, in epoch milliseconds.
    pub valid_until_ms: i64,
}

/// How a caller authenticates, over the process's own state.
#[derive(Debug)]
pub struct Authenticator<'a> {
    /// The database the key and principal tables live in.
    pub database: &'a CoordDb,
    /// The process key cache, whose generations fence a mid-verification
    /// revocation.
    pub keys: &'a JwtKeyCache,
    /// The caller's clock. A parameter, so every bound is testable without
    /// sleeping.
    pub clock: VerifyClock,
    /// The server-side token ceiling in seconds. A signer asking for longer is
    /// clamped, not obeyed.
    pub jwt_max_age_secs: u64,
}

impl Authenticator<'_> {
    /// Verify a presented bearer and resolve it to exactly one principal.
    ///
    /// The order is fixed and is the contract: parse, read the key, verify the
    /// signature and the claims, re-check the generation, then resolve the
    /// principal. A failure at any step is a `401` for the peer and a precise
    /// reason for the log -- never both a refusal and a socket close, because the
    /// close is the oracle the worker upgrade exists to withhold
    /// (`apps/coord/src/workers/worker-ws-upgrade.ts:84-102`).
    pub async fn authenticate(&self, token: &str) -> Result<AuthenticatedCaller, AuthFailure> {
        let parts = crate::auth::jwt_claims::JwtParts::split(token).map_err(map_malformed)?;
        let header = parts.header().map_err(map_malformed)?;

        let stored = load_authorized_key(self.database, &header.kid)
            .await
            .map_err(map_database)?;
        let Some(stored) = stored else {
            return Err(AuthFailure::UnknownKid { kid: header.kid });
        };
        let key: PublicKey =
            public_key_of(&stored).map_err(|error| AuthFailure::StoredKey(error.reason))?;

        let observed_generation = self.keys.generation(&header.kid);
        let context = VerifyContext {
            keys: self.keys,
            clock: self.clock,
            jwt_max_age_secs: self.jwt_max_age_secs,
        };
        let verified = verify_token(token, key, &context)
            .map_err(|error| AuthFailure::Verify(error.to_string()))?;

        // The principal read is a second await, and it is the longest step: a
        // revocation that lands during it must still refuse. Checking once more
        // here is the fourth of the four checks, and the only one whose window
        // is a database round trip rather than a signature verification.
        if !self
            .keys
            .generation_is_current(&header.kid, observed_generation)
        {
            return Err(AuthFailure::NoPrincipal { kid: header.kid });
        }

        let principal = resolve_key_principal(self.database, &header.kid)
            .await
            .map_err(map_database)?;
        let Some(principal) = principal else {
            return Err(AuthFailure::NoPrincipal { kid: header.kid });
        };

        Ok(AuthenticatedCaller {
            fingerprint: verified.fingerprint,
            label: stored.label,
            principal,
            key_generation: observed_generation,
            valid_until_ms: verified.valid_until_ms,
        })
    }
}

fn map_malformed(error: roost_protocol::ProtocolError) -> AuthFailure {
    AuthFailure::Malformed(error.reason)
}

fn map_database(error: roost_protocol::ProtocolError) -> AuthFailure {
    AuthFailure::Database(error.reason)
}
